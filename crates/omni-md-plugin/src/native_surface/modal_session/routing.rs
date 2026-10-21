//! Routes standard modal Undo/Redo through the actual field editor's public responder chain and existing native history.

use super::*;
use objc2_foundation::NSUndoManager;

/// Records the initially connected roles weakly; unsafe_unretained responder edges must not become additional strong owners.
struct RouteBinding {
    editor: Weak<NSTextView>,
    original_next: Option<Weak<NSResponder>>,
}

#[derive(Default)]
/// Keeps one route record and bounded reentry guards, without owning an editor, manager, or document.
pub(super) struct ModalUndoRoute {
    binding: RefCell<Option<Rc<RouteBinding>>>,
    installing: Cell<bool>,
    validating: Cell<bool>,
    performing: Cell<bool>,
}

/// Releases only this stack's reentry claim after native calls; Drop never changes input or history.
struct BusyGuard<'a>(&'a Cell<bool>);

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        // Only the successful outer claimant creates this guard; a rejected nested call cannot clear its Busy state.
        self.0.set(false);
    }
}

/// Rechecks pure Rust authority after native calls, so getter/retain reentry cannot return an accepted-cleanup snapshot as live.
fn local_permission(owner: &NativeMarkdownModalInput) -> bool {
    owner.ivars().active.get()
        && MODAL_ACTIVE.with(Cell::get) == Some(owner.ivars().generation)
        && owner.ivars().provider.permits_modal_command()
        && owner.ivars().undo.preflight().is_ok()
        && owner.ivars().borrowed.failure().is_none()
        && owner.ivars().failure.borrow().is_none()
}

/// Authorizes only this dialog's active actual client; all Rust borrows end before native getters.
fn active_editor(owner: &NativeMarkdownModalInput) -> Option<Retained<NSTextView>> {
    if !owner.is_current() || !local_permission(owner) {
        // Stale, closing, construction/finish Busy, and ownership failure preserve input without granting execution authority.
        return None;
    }
    let field = owner.ivars().field.get()?.load()?;
    let window = owner.ivars().window.get()?.load()?;
    let editor = field.currentEditor()?.downcast::<NSTextView>().ok()?;
    let concrete = editor.downcast_ref::<NativeMarkdownSessionFieldEditor>()?;
    if !concrete.belongs_to(&owner.ivars().undo, &owner.ivars().ledger)
        || !field
            .window()
            .is_some_and(|actual| std::ptr::eq(&*actual, &*window))
        || !editor
            .window()
            .is_some_and(|actual| std::ptr::eq(&*actual, &*window))
        || !window.firstResponder().is_some_and(|actual| {
            // Compares live identities only; currentEditor and firstResponder must name the same concrete input editor.
            std::ptr::eq(
                AsRef::<AnyObject>::as_ref(&*actual),
                AsRef::<AnyObject>::as_ref(&*editor),
            )
        })
    {
        // A default/foreign editor, unfocused field, or other window cannot operate this dialog's history.
        return None;
    }
    let application = NSApplication::sharedApplication(owner.mtm());
    if !application
        .keyWindow()
        .is_some_and(|actual| std::ptr::eq(&*actual, &*window))
        || !application
            .modalWindow()
            .is_some_and(|actual| std::ptr::eq(&*actual, &*window))
    {
        // The main document or background window cannot use menu or delayed actions to cross this modal history boundary.
        return None;
    }
    // SAFETY: The actual NSTextView is live on the main thread; the public getter reads marked state without committing or cancelling IME input.
    let marked: bool = unsafe { msg_send![&*editor, hasMarkedText] };
    if marked || !local_permission(owner) {
        // AppKit/IME must finish its own composition semantics before whole-field history may execute.
        return None;
    }
    Some(editor)
}

/// Revalidates after each native setter; compensation uses the outer weak record even if reentry already took the shared slot.
fn install_steps(
    attach_owner: impl FnOnce(),
    still_current: impl Fn() -> bool,
    attach_editor: impl FnOnce(),
    rollback: impl FnOnce(),
) {
    attach_owner();
    if !still_current() {
        // The first setter may reenter accepted finish; never attach the editor to that finished owner afterward.
        rollback();
        return;
    }
    attach_editor();
    if !still_current() {
        // The second setter may write its edge after finish detached it; the local record still permits removing this late edge.
        rollback();
    }
}

/// Installs once after actual begin/change; later AppKit rewiring disables this route without adding another route.
pub(super) fn install_after_editing(owner: &NativeMarkdownModalInput) {
    if owner.ivars().routing.installing.replace(true) {
        // A setter may reenter the delegate; only the outer installation owns publication.
        return;
    }
    let _installing = BusyGuard(&owner.ivars().routing.installing);
    if owner.ivars().routing.binding.borrow().is_some() {
        // Never rewire an existing installation; later checks reject AppKit changes rather than replacing its new successor.
        return;
    }
    let Some(editor) = active_editor(owner) else {
        // Begin may precede final firstResponder installation; a later actual change may try again, without timed retries.
        return;
    };
    // SAFETY: Both responders are live on this stack; the existing AppKit window/control graph keeps the original successor alive.
    let original = unsafe { editor.nextResponder() };
    let self_responder: &NSResponder = owner;
    if original
        .as_ref()
        .is_some_and(|next| std::ptr::eq(&**next, self_responder))
    {
        // A self edge without our record is unknown chain state; reject rather than creating a cycle or guessing the original successor.
        return;
    }
    let binding = Rc::new(RouteBinding {
        editor: Weak::from_retained(&editor),
        original_next: original.as_ref().map(Weak::from_retained),
    });
    if !owner.is_current() || !local_permission(owner) {
        // Weak construction and the successor getter have returned; reentrant finish forbids publishing a record or changing edges.
        return;
    }
    // Publishes the pure Rust weak record before setters; installing blocks reentry, and no RefCell borrow crosses a native message.
    let previous = owner
        .ivars()
        .routing
        .binding
        .replace(Some(Rc::clone(&binding)));
    drop(previous);
    install_steps(
        || {
            // SAFETY: The dialog retains owner; a local claim and the original window graph retain the successor, while the edge itself is unretained.
            unsafe { owner.setNextResponder(original.as_deref()) };
        },
        || {
            // Completes the reentrant parent upgrade first, then checks pure authority and this Rc identity; an empty slot is not publication.
            owner.is_current()
                && local_permission(owner)
                && owner
                    .ivars()
                    .routing
                    .binding
                    .borrow()
                    .as_ref()
                    .is_some_and(|current| {
                        // Rc identity grants only Rust-record publication; each native role still requires its actual weak reference.
                        Rc::ptr_eq(current, &binding)
                    })
        },
        || {
            // SAFETY: This stack remains authorized after the first setter; the retained owner covers this unretained chain edge.
            unsafe { editor.setNextResponder(Some(owner)) };
        },
        || {
            // The complete local weak record removes our late edge even if reentrant finish already took the shared slot.
            restore_binding(owner, &binding);
            let remains_ours =
                owner
                    .ivars()
                    .routing
                    .binding
                    .borrow()
                    .as_ref()
                    .is_some_and(|current| {
                        // Do not take a different installation published later.
                        Rc::ptr_eq(current, &binding)
                    });
            if remains_ours {
                // Removes only this Rust record; dropping outside the borrow creates no new native owner.
                drop(owner.ivars().routing.binding.take());
            }
        },
    );
}

/// Checks actual weak roles without inferring liveness from old addresses or replacing a later AppKit chain.
fn binding_is_current(owner: &NativeMarkdownModalInput, editor: &NSTextView) -> bool {
    let binding = owner
        .ivars()
        .routing
        .binding
        .borrow()
        .as_ref()
        .map(Rc::clone);
    let Some(binding) = binding else {
        // An absent completed route grants no history-action authority.
        return false;
    };
    if !binding
        .editor
        .load()
        .is_some_and(|expected| std::ptr::eq(&*expected, editor))
    {
        // The old record cannot authorize a changed shared editor or client.
        return false;
    }
    let original = match &binding.original_next {
        Some(next) => match next.load() {
            // The upgraded successor covers unsafe chain getters below; expiration must not be confused with an originally nil edge.
            Some(next) => Some(next),
            // An expired original successor leaves chain identity unproven; preserve input and reject the action.
            None => return false,
        },
        // Only an originally nil successor permits a current nil; a later expired weak reference does not.
        None => None,
    };
    // SAFETY: Editor and owner are live; the original successor is upgraded or originally nil, and only public chain roles are compared.
    let (actual_next, owner_next) = unsafe { (editor.nextResponder(), owner.nextResponder()) };
    actual_next.as_ref().is_some_and(|next| {
        // Compares actual live responder identities without exposing addresses.
        std::ptr::eq(&**next, AsRef::<NSResponder>::as_ref(owner))
    }) && match (owner_next, original) {
        // The original command successor must remain intact rather than redirecting to a parent or another panel.
        (Some(actual), Some(expected)) => std::ptr::eq(&*actual, &*expected),
        // An original chain endpoint remains an endpoint.
        (None, None) => true,
        // AppKit or another owner changed the successor; reject history without taking the chain back.
        _ => false,
    }
}

/// Returns a cached history claim for this action stack only; never constructs a missing manager.
fn current_manager(owner: &NativeMarkdownModalInput) -> Option<Retained<NSUndoManager>> {
    if owner.ivars().routing.installing.get() || owner.ivars().routing.performing.get() {
        // Installation or history execution rejects nested requests without clearing history or poisoning the owner.
        return None;
    }
    let editor = active_editor(owner)?;
    if !binding_is_current(owner, &editor) {
        // Read-only menu validation cannot allocate missing history or repair an invalid route through another path.
        return None;
    }
    // The cached entry checks its main-thread slot before the existing take/retain/restore sequence; it creates no missing history.
    let manager = owner.ivars().undo.cached_manager(&owner.ivars().ledger)?;
    let actual = editor.undoManager()?;
    if !std::ptr::eq(&*manager, &*actual)
        || !owner.ivars().undo.owns_observed_manager(&manager)
        || manager.isUndoing()
        || manager.isRedoing()
        || manager.groupingLevel() > 1
        || active_editor(owner).is_none()
        || !binding_is_current(owner, &editor)
        || !local_permission(owner)
    {
        // Native getters may reenter, so authority is checked again; nested groups are not the top-level group undo() may close.
        return None;
    }
    Some(manager)
}

/// Standard menu validation only reports actual stack availability; it does not update menus, end groups, or send actions.
pub(super) fn can_perform(owner: &NativeMarkdownModalInput, redo: bool) -> bool {
    if owner.ivars().routing.validating.replace(true) {
        // canRedo may emit checkpoint and synchronously reenter validation; the inner rejection cannot clear the outer claim.
        return false;
    }
    let _validating = BusyGuard(&owner.ivars().routing.validating);
    let Some(manager) = current_manager(owner) else {
        // Missing authorized input or history disables this action without using the alert's default manager.
        return false;
    };
    if redo {
        // Native canRedo may emit checkpoint; rechecks session and route authority for the same input before returning.
        manager.canRedo() && current_manager(owner).is_some()
    } else {
        // canUndo proves history exists; group, execution, and marked-input checks separately authorize its execution.
        manager.canUndo() && current_manager(owner).is_some()
    }
}

/// Keyboard and menu share public actions on the verified native manager, without synthesizing input or history.
pub(super) fn perform(owner: &NativeMarkdownModalInput, redo: bool) {
    if owner.ivars().routing.validating.get() {
        // Read-only menu checkpoint cannot reenter history mutation; the user may act again after outer validation returns.
        return;
    }
    let Some(manager) = current_manager(owner) else {
        // Late, unfocused, IME, or Busy requests are no-ops that preserve user input and existing Undo.
        return;
    };
    let available = if redo {
        // Queries Redo independently without altering native grouping configuration.
        manager.canRedo()
    } else {
        // Foundation undo() may close its level-one event group itself; do not call endUndoGrouping here.
        manager.canUndo()
    };
    if !available
        || current_manager(owner).is_none()
        || owner.ivars().routing.performing.replace(true)
    {
        // Checkpoint/getter reentry may revoke authority; rejection still precedes all history mutation.
        return;
    }
    let _performing = BusyGuard(&owner.ivars().routing.performing);
    if !local_permission(owner) {
        // current_manager releases native local claims on return; if their drop reentered closure, reject without further native queries.
        return;
    }
    if redo {
        // The sole registered manager replays actual NSTextView entries, with no Rust borrow spanning native callbacks.
        manager.redo();
    } else {
        // Standard undo owns its public top-level grouping; actual didChange retains normal input and ownership observation.
        manager.undo();
    }
}

/// Cleanup and installation compensation restore actual edges through the same weak record, even after reentry takes the shared slot.
fn restore_binding(owner: &NativeMarkdownModalInput, binding: &RouteBinding) {
    // All RefCell borrows have ended; temporary claims cover one detach rather than surviving into a later session.
    let editor = binding.editor.load();
    let original = binding.original_next.as_ref().and_then(Weak::load);
    if let Some(editor) = editor {
        // SAFETY: This stack keeps editor and owner alive; inspecting the current chain prevents overwriting another owner's new edge.
        let current = unsafe { editor.nextResponder() };
        if current.as_ref().is_some_and(|next| {
            // Restore the edge only while this node remains the actual next responder.
            std::ptr::eq(&**next, AsRef::<NSResponder>::as_ref(owner))
        }) {
            // SAFETY: A local claim retains any live original successor; otherwise detaches instead of reusing an expired address.
            unsafe { editor.setNextResponder(original.as_deref()) };
        }
    }
    // SAFETY: This node belongs to the dialog; removing its unretained successor revokes access to the old window on exit.
    unsafe { owner.setNextResponder(None) };
}

/// Accepted cleanup removes this node and restores the original editor successor only while the actual edge still belongs to this installation.
pub(super) fn detach(owner: &NativeMarkdownModalInput) {
    let binding = owner.ivars().routing.binding.take();
    if let Some(binding) = binding {
        // Clearing the shared slot prevents reentrant commands while the local Rc preserves the weak restoration record.
        restore_binding(owner, &binding);
    }
}
