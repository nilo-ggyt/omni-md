//! Unpublished installation rolls back its own actual relationships without relying on final View
//! destruction or taking successor layers.

use super::*;

#[cfg(feature = "native-markdown-runtime-probe")]
pub(super) mod probe;

pub(super) struct Installation<'a> {
    view: &'a NativeMarkdownPagedTextView,
    backing: Option<Retained<CALayer>>,
    mount: Option<Retained<CALayer>>,
    armed: bool,
}
impl<'a> Installation<'a> {
    pub(super) fn new(view: &'a NativeMarkdownPagedTextView) -> Self {
        Self {
            view,
            backing: None,
            mount: None,
            armed: true,
        }
    }
    /// Enable layer backing and retain the actual result for precise unpublished rollback.
    pub(super) fn enable(&mut self, origin: &Origin) -> Result<Retained<CALayer>> {
        if !origin.current(self.view) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        self.view.setWantsLayer(true);
        self.backing = self.view.layer(); // Record the actual backing produced by this setter before the post-call current check can revoke authority.
        if !origin.current(self.view) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        self.backing
            .clone()
            .ok_or(NativeMarkdownPagedTextViewError::ContextUnavailable)
    }
    pub(super) fn mount(&mut self, claim: Retained<CALayer>) {
        self.mount = Some(claim);
    }
    pub(super) fn commit(&mut self) {
        self.armed = false;
    }
}
impl Drop for Installation<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        CATransaction::begin();
        let transaction = Transaction;
        CATransaction::setDisableActions(true);
        if let (Some(mount), Some(backing)) = (&self.mount, &self.backing) {
            let parent = mount.superlayer();
            let ours = parent
                .as_deref()
                .is_some_and(|parent| std::ptr::eq::<CALayer>(parent, &**backing));
            drop(parent);
            if ours {
                mount.removeFromSuperlayer(); // Detach this mount only while it still belongs to the original backing, leaving another owner layer untouched.
            }
            if mount.superlayer().is_none() {
                // SAFETY: Clear this unpublished, actually detached mount only, without document images
                // or successor-owner relationships.
                unsafe { mount.setSublayers(None) };
            }
        }
        // Native cleanup may reenter; recheck both actual backing identity and absence of a published
        // CPU owner.
        let unpublished = self.view.ivars().cpu.borrow().is_none();
        let actual = self.view.layer();
        let same = match (&actual, &self.backing) {
            (Some(actual), Some(backing)) => std::ptr::eq::<CALayer>(&**actual, &**backing),
            (None, None) => true, // Without an acquired backing, clean only the still-nil original construction relationship.
            _ => false, // A replacement layer or CPU owner prevents this rollback from taking its relationship.
        };
        drop(actual);
        if unpublished && same && self.view.ivars().cpu.borrow().is_none() {
            self.view.setWantsLayer(false);
            let actual = self.view.layer();
            let ours = actual.as_ref().is_some_and(|actual| {
                self.backing
                    .as_ref()
                    .is_some_and(|backing| std::ptr::eq::<CALayer>(&**actual, &**backing))
            });
            drop(actual);
            if ours && self.view.ivars().cpu.borrow().is_none() {
                self.view.setLayer(None);
            }
        }
        drop(transaction);
        CATransaction::flush(); // Flush actual rollback relationships even on unpublished failure, without relying on eventual framework cache cleanup.
    }
}
