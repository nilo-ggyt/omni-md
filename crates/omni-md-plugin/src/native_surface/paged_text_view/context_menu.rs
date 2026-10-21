//! The actual input client's Chinese Native edit menu reuses existing selectors/history without another command router.

use super::*;
use objc2::sel;
use objc2_app_kit::{NSMenuItem, NSPasteboard, NSPasteboardTypeFileURL, NSPasteboardTypeString};
use objc2_foundation::{NSString, ns_string};

impl NativeMarkdownPagedTextView {
    /// Each opening derives availability from current client selection/history without reading or committing composition candidates.
    pub(super) fn edit_context_menu(&self) -> Result<Retained<NSMenu>> {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        self.stop_drag_scroll(); // Menu tracking ends the old drag's viewport timer lifetime.
        self.close_context_menu(); // Do not accumulate menus; extract the owner before AppKit calls so actual close may reenter.
        let origin = self.origin()?;
        let client = origin.delegate.context_menu_client()?;
        if !origin.current(self) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        if let Some(client) = client {
            return client.edit_context_menu(); // A parent-background context click still edits the original active Cell, never the parent document's history.
        }
        let undo = origin
            .input
            .can_perform_history(NativeMarkdownPagedInputHistory::Undo)?;
        let redo = origin
            .input
            .can_perform_history(NativeMarkdownPagedInputHistory::Redo)?;
        let editing = origin.state.marked.is_none();
        let selected = origin.state.selection_range_count != 0
            && (origin.state.selection_range_count > 1 || origin.state.primary_range.length != 0);
        let pasteboard = origin.step(self, NSPasteboard::generalPasteboard)?;
        // SAFETY: The public type constants describe accepted formats only; this menu reads no clipboard text/file paths.
        let native_types = unsafe { [NSPasteboardTypeString, NSPasteboardTypeFileURL] };
        let types = NSArray::from_slice(if origin.input.is_cell() {
            &native_types[..1] // Cell accepts plain text only; workspace Body owns file import, and menus do not extend that capability.
        } else {
            &native_types
        });
        let paste = origin
            .step(self, || pasteboard.availableTypeFromArray(&types))?
            .is_some();
        let menu = origin.step(self, || {
            NSMenu::initWithTitle(NSMenu::alloc(self.mtm()), ns_string!("编辑"))
        })?;
        origin.step(self, || menu.setAutoenablesItems(false))?;
        // Empty keyEquivalent preserves existing bindings; the context entry registers no global/application shortcuts.
        for (index, (title, action, enabled)) in [
            ("撤销", sel!(undo:), undo),
            ("重做", sel!(redo:), redo),
            ("剪切", sel!(cut:), editing && selected),
            ("复制", sel!(copy:), editing && selected),
            ("粘贴", sel!(paste:), editing && paste),
            (
                "全选",
                sel!(selectAll:),
                editing && origin.state.utf16_len != 0,
            ),
        ]
        .into_iter()
        .enumerate()
        {
            if index == 2 || index == 5 {
                let separator = origin.step(self, || NSMenuItem::separatorItem(self.mtm()))?;
                origin.step(self, || menu.addItem(&separator))?;
            }
            let label = origin.step(self, || NSString::from_str(title))?;
            // SAFETY: This NSResponder implements every fixed action with the standard single-sender menu ABI.
            let item = origin.step(self, || unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(self.mtm()),
                    &label,
                    Some(action),
                    ns_string!(""),
                )
            })?;
            // SAFETY: Menu target weakly borrows this View; begin_close revokes each action/target before releasing View.
            origin.step(self, || unsafe { item.setTarget(Some(self)) })?;
            origin.step(self, || item.setEnabled(enabled))?;
            origin.step(self, || menu.addItem(&item))?;
        }
        if !origin.current(self) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        *self.ivars().context_menu.borrow_mut() = Some(menu.clone());
        Ok(menu) // AppKit presents/tracks; View owns only this bounded menu, without text or another history snapshot.
    }

    /// A closed/replaced menu stops tracking immediately and loses all action authority before client release.
    pub(super) fn close_context_menu(&self) {
        let menu = self.ivars().context_menu.borrow_mut().take();
        if let Some(menu) = menu {
            menu.cancelTrackingWithoutAnimation();
            // Visit only this menu's six actual actions/two separators, without modifying system/other-window menus.
            for item in menu.itemArray() {
                // SAFETY: Clear only public menu relationships installed by this View, with no text/active-input borrow.
                unsafe {
                    item.setAction(None);
                    item.setTarget(None);
                }
            }
        }
    }

    /// Close observes the actual client-created menu/items through weak identities without retaining tracking, Input, or document.
    pub(in crate::native_surface) fn context_menu_weaks(&self) -> Vec<objc2::rc::Weak<AnyObject>> {
        let menu = self.ivars().context_menu.borrow().clone();
        let mut roles = Vec::new();
        if let Some(menu) = menu {
            roles.push(objc2::rc::Weak::from(<NSMenu as AsRef<AnyObject>>::as_ref(
                &menu,
            )));
            // Run Native getters outside the slot borrow, returning actual item weak references for this one current menu.
            for item in menu.itemArray() {
                roles.push(objc2::rc::Weak::from(
                    <NSMenuItem as AsRef<AnyObject>>::as_ref(&item),
                ));
            }
        }
        roles
    }
}
