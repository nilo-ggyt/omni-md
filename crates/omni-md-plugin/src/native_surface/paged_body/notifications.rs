//! Actual clip-geometry changes wake the original continuation; scrolling never depends on an
//! incidental later draw.

use std::{
    ptr::NonNull,
    sync::{Arc, atomic::Ordering},
};

use block2::RcBlock;
use dispatch2::MainThreadBound;
use objc2::{rc::Weak, runtime::AnyObject};
use objc2_app_kit::{
    NSClipView, NSViewBoundsDidChangeNotification, NSViewFrameDidChangeNotification,
};
use objc2_foundation::{MainThreadMarker, NSNotification, NSNotificationCenter};

use super::{NativeMarkdownPagedBody, NativeMarkdownPagedBodyError, Result};

impl NativeMarkdownPagedBody {
    /// Retain exact observer tokens for this clip and use only weak Body references in callbacks.
    pub(super) fn observe_clip(&self, clip: &NSClipView) {
        let center = NSNotificationCenter::defaultCenter();
        // SAFETY: AppKit notification-name constants are process-long; observer objects are restricted
        // to the original clip.
        let names = unsafe {
            [
                NSViewBoundsDidChangeNotification,
                NSViewFrameDidChangeNotification,
            ]
        };
        let mut tokens = Vec::with_capacity(names.len());
        for name in names {
            let weak = MainThreadBound::new(self.weak.clone(), self.mtm);
            let off_main = Arc::clone(&self.off_main_notice);
            let callback = RcBlock::new(move |_notification: NonNull<NSNotification>| {
                let Some(mtm) = MainThreadMarker::new() else {
                    off_main.store(true, Ordering::Release);
                    return; // An off-main notification permanently revokes authority without touching Rc, AppKit, or layout state.
                };
                let Some(owner) = weak.get(mtm).upgrade() else {
                    return;
                };
                if !owner.active.get() {
                    return;
                }
                owner.cancel_mode_for_external_clip(); // Real clip intent outside preparation revokes the old mode position, preventing it from pulling the user back.
                // The existing Viewport observer synchronizes actual geometry; this observer only
                // queues the same main-thread continuation.
                // This avoids registration-order dependencies and mixed pre/post-update coordinates in
                // the notification stack.
                if let Err(cause) = owner.schedule() {
                    owner.fail(cause);
                }
            });
            // SAFETY: object is the real main-thread clip; a None queue executes synchronously on the
            // notification thread.
            // The block verifies the main thread before accessing MainThreadBound/Rc, and close
            // unregisters its retained token.
            let token = unsafe {
                center.addObserverForName_object_queue_usingBlock(
                    Some(name),
                    Some(clip),
                    None,
                    &callback,
                )
            };
            tokens.push(token);
        }
        *self.notifications.borrow_mut() = tokens;
    }

    /// Detach tokens before Native removal, allowing callbacks without a live slot borrow.
    pub(super) fn remove_clip_observers(&self) {
        let tokens = std::mem::take(&mut *self.notifications.borrow_mut());
        let center = NSNotificationCenter::defaultCenter();
        for token in tokens {
            // SAFETY: Remove only this owner's retained tokens, leaving same-name system notifications
            // and other sessions intact.
            unsafe {
                center.removeObserver(<objc2::runtime::ProtocolObject<
                    dyn objc2_foundation::NSObjectProtocol,
                > as AsRef<AnyObject>>::as_ref(&*token))
            };
        }
    }

    /// List every Native role actually constructed by this Body; do not derive the ledger from
    /// whichever objects remain after close.
    pub fn native_weaks(&self) -> Result<Vec<(&'static str, Weak<AnyObject>)>> {
        let view = self.view()?;
        let viewport = self
            .viewport
            .borrow()
            .clone()
            .ok_or(NativeMarkdownPagedBodyError::Closed)?;
        let mut roles = Vec::new();
        let weak = |value: &AnyObject| Weak::from(value);
        roles.push(("body-view", weak(&view)));
        if let Some(typography) = self.typography.borrow().as_ref() {
            roles.push(("preview-font-metrics", weak(&typography.metrics_owner)));
        } // The finite system-line metrics owner also belongs to real close verification, outside neither the font nor body weak graph.
        if let Some(context) = view.inputContext() {
            roles.push(("input-context", weak(&context)));
        }
        let document = viewport.document_view()?;
        roles.push(("scroll-document", weak(&document)));
        for observer in viewport.observer_weaks()? {
            roles.push(("viewport-observer", observer));
        }
        let tokens = self
            .notifications
            .borrow()
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        for token in tokens {
            let actual =
                <objc2::runtime::ProtocolObject<dyn objc2_foundation::NSObjectProtocol> as AsRef<
                    AnyObject,
                >>::as_ref(&*token);
            roles.push(("body-observer", weak(actual)));
        }
        if let Some(layers) = view.cpu_layer_weaks() {
            roles.extend(layers.into_iter().map(|layer| ("cpu-role", layer)));
        }
        roles.extend(
            view.context_menu_weaks()
                .into_iter()
                .map(|menu| ("edit-menu", menu)),
        );
        if let Some(timer) = view.drag_scroll_weak() {
            roles.push(("drag-scroll-timer", timer));
        }
        // Temporary Rc handles move Native claims outside slot borrows; the returned ledger contains
        // weak references only.
        drop((view, viewport));
        Ok(roles)
    }
}
