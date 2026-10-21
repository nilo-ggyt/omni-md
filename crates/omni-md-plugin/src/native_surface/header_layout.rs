//! Weak references to the existing header controls. Synchronous layout owns no
//! document, timer, or authority over a successor window.

use super::*;

mod geometry;
pub(super) use geometry::editor_area_frames;
use geometry::header_frames;

/// Observe the two header rows, feedback, and three footer columns weakly.
/// The original construction ledger and controller retain teardown responsibility.
pub(super) struct NativeMarkdownHeaderControls {
    controls: [Weak<NSView>; 14],
}

/// Construction precedes publication of Active. Runtime layout additionally
/// requires the existing foreground owner; construction does not bypass close checks.
#[derive(Clone, Copy)]
pub(super) enum NativeMarkdownHeaderAuthority {
    Construction,
    Foreground,
}

impl NativeMarkdownHeaderControls {
    /// Apply the original route's undo/redo availability without owning history
    /// or an input field, preserving both actions and their established positions.
    pub(super) fn set_history_available(&self, available: [bool; 2], current: &dyn Fn() -> bool) {
        for (control, available) in self.controls[4..6].iter().zip(available) {
            if !current() {
                return; // Revoked document/input authority cannot upgrade an old live control.
            }
            let Some(control) = control
                .load()
                .and_then(|control| control.downcast::<NSControl>().ok())
            else {
                return; // A departed button is never replaced or reconstructed by layout.
            };
            if !current() {
                // Weak upgrade can enter Native. Revalidate before changing the
                // retained control rather than granting authority from liveness alone.
                return;
            }
            control.setEnabled(available);
        }
    }

    /// Observe only the format strip's actual scroll root; its original controls
    /// collection and view tree retain the individual buttons.
    pub(super) fn new(controls: [&NSView; 8], toolbar: [Weak<NSView>; 6]) -> Self {
        let [
            title,
            mode,
            menu,
            status,
            path,
            footer_mode,
            footer_save,
            footer_counts,
        ] = controls.map(Weak::new);
        let [undo, redo, find, info, copy, format] = toolbar;
        Self {
            controls: [
                title,
                mode,
                menu,
                status,
                undo,
                redo,
                find,
                info,
                copy,
                format,
                path,
                footer_mode,
                footer_save,
                footer_counts,
            ],
        }
    }

    /// Publish one content size per synchronous call. Reentrant resize or close
    /// immediately revokes permission for every remaining setter.
    pub(super) fn layout(
        &self,
        controller: &NativeMarkdownController,
        window: &objc2_app_kit::NSWindow,
        authority: NativeMarkdownHeaderAuthority,
    ) -> bool {
        if controller.ivars().close_finalizer_scheduled.get()
            || VERIFYING_NATIVE_MARKDOWN_CLOSE.with(Cell::get).is_some()
            || (matches!(authority, NativeMarkdownHeaderAuthority::Foreground)
                && !native_markdown_controller_has_foreground_authority(controller))
        {
            // Reject accepted/Closing ownership before the first Native getter.
            // Construction skips only the not-yet-published Active requirement.
            return false;
        }
        // This revision protects one stack-local publication. Exhaustion leaves
        // the current layout intact instead of wrapping into an old authority.
        let Some(revision) = controller
            .ivars()
            .header_layout_revision
            .get()
            .checked_add(1)
        else {
            return false;
        };
        controller.ivars().header_layout_revision.set(revision);
        let Some(content) = window.contentView() else {
            // A closed window has no content root; do not remount detached controls.
            return false;
        };
        let size = content.frame().size;
        let Some(frames) = header_frames(size.width, size.height) else {
            // Invalid sizes publish no partial geometry. The existing window
            // contract still owns the 900-by-600 minimum content size.
            return false;
        };
        let is_current = || {
            // Check scalar authority first: an old reentrant stack cannot keep
            // modifying the controls published by a later layout or close.
            if controller.ivars().close_finalizer_scheduled.get()
                || VERIFYING_NATIVE_MARKDOWN_CLOSE.with(Cell::get).is_some()
                || controller.ivars().header_layout_revision.get() != revision
                || (matches!(authority, NativeMarkdownHeaderAuthority::Foreground)
                    && !native_markdown_controller_has_foreground_authority(controller))
            {
                return false;
            }
            let Some(panel) = controller.ivars().editing_window.load() else {
                // Retained local controls cannot substitute for the departed weak window.
                return false;
            };
            let owned_window: &objc2_app_kit::NSWindow = &panel;
            if !std::ptr::eq(owned_window, window) {
                // A foreign window notification has no authority over this controller's header.
                return false;
            }
            if matches!(authority, NativeMarkdownHeaderAuthority::Foreground)
                && !super::controller::native_markdown_controller_owns_editing_window(
                    controller, window,
                )
            {
                // Only synchronous construction may precede Active publication;
                // an actual resize must belong to the original editing-window owner.
                return false;
            }
            let Some(current_content) = window.contentView() else {
                // Reentrant close may remove the content root; old frames cannot remount it.
                return false;
            };
            if !std::ptr::eq(&*content, &*current_content) || current_content.frame().size != size {
                // A changed content identity or size invalidates the complete old
                // result. The new resize owns its normal synchronous publication.
                return false;
            }
            // The getters above may also reenter. Recheck revision and close
            // facts without retaining a TLS borrow across any Native operation.
            !controller.ivars().close_finalizer_scheduled.get()
                && VERIFYING_NATIVE_MARKDOWN_CLOSE.with(Cell::get).is_none()
                && controller.ivars().header_layout_revision.get() == revision
                && (!matches!(authority, NativeMarkdownHeaderAuthority::Foreground)
                    || native_markdown_controller_has_foreground_authority(controller))
        };

        for (control, frame) in self.controls.iter().zip(frames.0) {
            if !is_current() {
                // Accepted teardown forbids upgrading even a still-live weak
                // control, so this path cannot extend the old release barrier.
                return false;
            }
            // Retain at most the control needed for one setter, avoiding a whole
            // toolbar of temporary strong references across layout/close callbacks.
            let Some(control) = control.load() else {
                // A missing constructed control stops publication; no substitute
                // may hide the original owner's close or invalidated graph.
                return false;
            };
            if !is_current() {
                // Do not queue a retry: the newer resize or close owns the successor work.
                return false;
            }
            control.setFrame(NSRect::new(
                NSPoint::new(frame.x, frame.y),
                NSSize::new(frame.width, frame.height),
            ));
            if !is_current() {
                // Setter reentry revokes the remaining old batch before it can
                // overwrite controls belonging to a newer complete layout.
                return false;
            }
        }
        true
    }
}
