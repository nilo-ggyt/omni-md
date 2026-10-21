//! An independent Native-installation compensation ticket retains original images and exact
//! relationship identities even after reentrant cleanup takes payload.

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in super::super) enum AssignmentPoint {
    Image,
    SecondaryImage,
    Background,
    ForegroundColor,
    SecondColor,
    Mask,
    SecondMask,
    Children,
}

impl AssignmentPoint {
    fn bit(self) -> u16 {
        1 << self as u8
    }
}

pub(super) struct Assignment<'a> {
    slot: &'a Slot,
    token: Rc<()>,
    image: CFRetained<CGImage>,
    secondary: Option<CFRetained<CGImage>>,
    presentation: PixelPresentation,
    colors: Option<PairedColors>,
    effects: Cell<u16>,
    committed: bool,
    #[cfg(feature = "native-markdown-runtime-probe")]
    probe: Option<&'a dyn Fn(AssignmentPoint)>,
}

impl<'a> Assignment<'a> {
    pub(super) fn begin(
        slot: &'a Slot,
        image: &CFRetained<CGImage>,
        secondary: Option<&CFRetained<CGImage>>,
        presentation: PixelPresentation,
        palette: &Palette,
    ) -> Result<Self> {
        let paired = matches!(presentation, PixelPresentation::OpaquePairedMasks { .. });
        if paired != secondary.is_some() {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        if slot.payload.borrow().is_some()
            || slot
                .installing
                .borrow()
                .as_ref()
                .is_some_and(|weak| weak.upgrade().is_some())
        {
            // Even after reentrant clear takes payload, the original guard exclusively owns this
            // installation stack; reject successor slot admission,
            // because a returning old Native setter could overwrite relationships already published by
            // a new token.
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let image = image.clone();
        let secondary = secondary.cloned();
        let colors = if presentation == PixelPresentation::OpaqueNeutralMask {
            Some(PairedColors {
                background: palette.white.clone(),
                foreground: palette.black.clone(),
                secondary_foreground: palette.black.clone(),
            })
        } else {
            None
        }; // Acquire and release all CF claims outside RefCell borrows, including failure exits.
        let token = Rc::new(());
        slot.installing.replace(Some(Rc::downgrade(&token)));
        Ok(Self {
            slot,
            token,
            image,
            secondary,
            presentation,
            colors,
            effects: Cell::new(0),
            committed: false,
            #[cfg(feature = "native-markdown-runtime-probe")]
            probe: None,
        })
    }

    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn set_probe(&mut self, probe: &'a dyn Fn(AssignmentPoint)) {
        self.probe = Some(probe);
    }

    pub(super) fn owns(&self, token: &Rc<()>) -> bool {
        Rc::ptr_eq(&self.token, token)
    }

    fn active(&self) -> bool {
        self.slot
            .installing
            .borrow()
            .as_ref()
            .is_some_and(|weak| weak.ptr_eq(&Rc::downgrade(&self.token)))
    }

    fn require_payload(&self) -> Result<()> {
        let same = self.active()
            && self
                .slot
                .payload
                .borrow()
                .as_ref()
                .is_some_and(|value| self.owns(&value.token));
        if same {
            Ok(())
        } else {
            Err(NativeMarkdownPagedTextViewError::Revoked)
        }
    }

    pub(super) fn capture_colors(&mut self, colors: Option<&PairedColors>) {
        if let Some(colors) = colors {
            self.colors = Some(PairedColors {
                background: colors.background.clone(),
                foreground: colors.foreground.clone(),
                secondary_foreground: colors.secondary_foreground.clone(),
            });
        } // Paired colors replace the initial empty claim only; neutral palette identity remains original, with CF retain/release outside slot borrows.
    }

    pub(super) fn publish_payload(&self, colors: Option<PairedColors>) -> Result<()> {
        if !self.active() {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let mut payload = self.slot.payload.borrow_mut();
        if payload.is_some() {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        *payload = Some(Payload {
            token: Rc::clone(&self.token),
            image: None,
            secondary_image: None,
            presentation: self.presentation,
            paired_colors: colors,
        }); // Move already-retained colors and the Rust token only, without Native calls or CF retain.
        Ok(())
    }

    /// Record the exact setter obligation before calling Native, then require the same token after
    /// every reentrant boundary.
    pub(super) fn apply(
        &self,
        point: AssignmentPoint,
        current: &impl Fn() -> bool,
        native: impl FnOnce(),
    ) -> Result<()> {
        self.require_payload()?;
        checked(current, || {
            // The pre-call current callback may take payload; recheck the original ticket before
            // recording the immediately following setter obligation.
            self.require_payload()?;
            self.effects.set(self.effects.get() | point.bit());
            native();
            #[cfg(feature = "native-markdown-runtime-probe")]
            if let Some(probe) = self.probe {
                probe(point); // The diagnostic callback runs immediately after the Native effect, before persistent image/installed-field registration.
            }
            self.require_payload()
        })??;
        // Post-call current may also reenter cleanup; true cannot authorize registration into an
        // already-taken payload.
        self.require_payload()
    }

    pub(super) fn commit(mut self) -> Result<()> {
        self.require_payload()?;
        self.committed = true;
        Ok(()) // The actual complete payload takes long-term claims; the local ticket returns this installation admission only.
    }

    fn touched(&self, point: AssignmentPoint) -> bool {
        self.effects.get() & point.bit() != 0
    }

    // The independent guard can compensate effects not yet entered in payload, but only for its
    // original active token.
    fn rollback(&self) {
        if !self.active() {
            return; // A successor token does not belong to this ticket; an old guard cannot clear its transferred relationships.
        }
        let payload = {
            let mut payload = self.slot.payload.borrow_mut();
            if payload
                .as_ref()
                .is_some_and(|value| !self.owns(&value.token))
            {
                return;
            }
            payload.take()
        }; // An empty payload still permits compensation for effects not yet registered; the guard independently retains original strong claims.
        self.slot.background_installed.set(false);
        self.slot.foreground_color_installed.set(false);
        self.slot.second_color_installed.set(false);
        self.slot.mask_installed.set(false);
        self.slot.second_mask_installed.set(false);
        self.slot.foreground_installed.set(false);
        self.slot.second_foreground_installed.set(false);

        if self.touched(AssignmentPoint::Image) {
            let target = match self.presentation {
                PixelPresentation::Image => &*self.slot.container,
                PixelPresentation::OpaqueNeutralMask
                | PixelPresentation::OpaquePairedMasks { .. } => self.slot.primary().1,
            };
            clear_image(target, &self.image);
        }
        if self.touched(AssignmentPoint::SecondaryImage)
            && let Some(image) = &self.secondary
        {
            clear_image(self.slot.secondary().1, image);
        }
        if self.touched(AssignmentPoint::Mask) {
            clear_mask(self.slot.primary().0, self.slot.primary().1);
        }
        if self.touched(AssignmentPoint::SecondMask) {
            clear_mask(self.slot.secondary().0, self.slot.secondary().1);
        }
        if self.touched(AssignmentPoint::Children) {
            clear_parent(self.slot.primary().0, &self.slot.container);
            if matches!(
                self.presentation,
                PixelPresentation::OpaquePairedMasks { .. }
            ) {
                clear_parent(self.slot.secondary().0, &self.slot.container);
            }
        }
        if let Some(colors) = &self.colors {
            if self.touched(AssignmentPoint::Background) {
                clear_color(&self.slot.container, &colors.background);
            }
            if self.touched(AssignmentPoint::ForegroundColor) {
                clear_color(self.slot.primary().0, &colors.foreground);
            }
            if self.touched(AssignmentPoint::SecondColor) {
                clear_color(self.slot.secondary().0, &colors.secondary_foreground);
            }
        }
        drop(payload); // Return CF claims and the original token outside all RefCell and business-state borrows.
    }
}

impl Drop for Assignment<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.rollback();
        }
        if self.active() {
            self.slot.installing.borrow_mut().take();
        } // Complete original-relationship compensation before returning admission; field CF claims then drop outside slot borrows.
    }
}

fn clear_image(layer: &CALayer, expected: &CGImage) {
    // Rollback needs scalar identity only; a local pool returns getter temporaries without retaining
    // old images in an outer event pool.
    let same = objc2::rc::autoreleasepool(|_| {
        // SAFETY: Read opaque identity while the original strong claim lives, without converting or
        // dereferencing an external object as CGImage.
        let actual = unsafe { layer.contents() };
        actual.as_deref().is_some_and(|value| {
            std::ptr::from_ref(value).cast::<()>() == std::ptr::from_ref(expected).cast::<()>()
        })
    });
    if same {
        // SAFETY: Clear the Native relationship only while it still matches this original image,
        // preserving external replacements.
        unsafe { layer.setContents(None) };
    }
}

fn clear_mask(foreground: &CALayer, mask: &CALayer) {
    let actual = foreground.mask();
    let same = actual
        .as_deref()
        .is_some_and(|value| std::ptr::eq(value, mask));
    drop(actual);
    if same {
        // SAFETY: The original fixed-mask identity still matches; an externally replaced mask does not
        // belong to this installation ticket.
        unsafe { foreground.setMask(None) };
    }
}

fn clear_parent(layer: &CALayer, parent: &CALayer) {
    if parent_is(layer, parent) {
        layer.removeFromSuperlayer();
    } // Detach only the original child in this container, without reclaiming an externally reparented relationship.
}

fn clear_color(layer: &CALayer, expected: &CGColor) {
    let actual = layer.backgroundColor();
    let same = actual
        .as_deref()
        .is_some_and(|value| std::ptr::eq(value, expected));
    drop(actual);
    if same {
        layer.setBackgroundColor(None);
    } // Equal RGB with another CGColor identity belongs elsewhere; equal values cannot erase actual replacement.
}
