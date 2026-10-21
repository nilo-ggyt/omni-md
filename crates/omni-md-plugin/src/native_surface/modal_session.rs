//! Owns modal-stack admission, isolated standard input, and public cleanup; file and document operations remain with their use cases.

use super::*;
use objc2_app_kit::{
    NSComboBoxCell, NSComboBoxDelegate, NSResponder, NSText, NSTextFieldCell,
    NSUserInterfaceValidations, NSValidatedUserInterfaceItem, NSWindow,
};

mod routing;

/// Bounds native UTF-16 input before conversion and UTF-8 bytes afterward; rejects the whole value without truncation.
const MAX_MODAL_INPUT_UNITS: usize = 16_384;
const MAX_MODAL_INPUT_BYTES: usize = 16_384;

thread_local! {
    /// Stores only a finite epoch; guards and cells separately prove controller liveness through weak owners.
    static MODAL_ACTIVE: Cell<Option<u64>> = const { Cell::new(None) };
    static MODAL_GENERATION: Cell<u64> = const { Cell::new(0) };
}

#[derive(Debug, Error)]
/// Cancellation is a normal response. Failed resignation preserves the input and grants no file mutation or accepted cleanup.
pub(super) enum NativeMarkdownModalError {
    #[error("另一笔记对话框仍在完成中")]
    Busy,
    #[error("笔记对话框所属会话已结束")]
    StaleOwner,
    #[error("无法创建新的笔记对话框，请重新打开笔记窗口")]
    Exhausted,
    #[error("对话框输入超过长度上限")]
    InputTooLong,
    #[error("系统文件选择器无法打开")]
    Aborted,
    #[error("笔记对话框返回了无法识别的结果：{0}")]
    UnexpectedResponse(isize),
    #[error("{0}")]
    Boundary(String),
}

/// Checks scalar admission before save, flush, owner removal, or new work; invokes no native getter.
pub(super) fn preflight_native_markdown_modal() -> Result<(), String> {
    if MODAL_ACTIVE.with(Cell::get).is_some() {
        // The dialog still owns its input, candidate, and local resources; closing cannot first alter the parent draft or authority.
        return Err(NativeMarkdownModalError::Busy.to_string());
    }
    Ok(())
}

/// Claims the scalar epoch before native construction; exhaustion and nesting preserve the accepted epoch.
fn claim_modal_generation() -> Result<u64, NativeMarkdownModalError> {
    if MODAL_ACTIVE.with(Cell::get).is_some() {
        // The existing modal remains exclusive; rejection must neither advance the epoch nor replace its claim.
        return Err(NativeMarkdownModalError::Busy);
    }
    let next = MODAL_GENERATION.with(|generation| {
        generation
            .get()
            .checked_add(1)
            .ok_or(NativeMarkdownModalError::Exhausted)
    })?;
    MODAL_GENERATION.with(|generation| generation.set(next));
    MODAL_ACTIVE.with(|active| active.set(Some(next)));
    Ok(next)
}

/// Releases only the matching claim; an older guard or repeated finish cannot revoke a newer modal.
fn release_modal_generation(generation: u64) {
    MODAL_ACTIVE.with(|active| {
        if active.get() == Some(generation) {
            // Only restores scalar admission; native and business owners must already have ended inside the caller's local pool.
            active.set(None);
        }
    });
}

/// Receives a controller already upgraded from a native weak reference; the borrowed identity supplements stack authority only.
fn modal_parent_is_current(parent: &NativeMarkdownController) -> bool {
    native_markdown_controller_is_current(parent)
        || BORROWED_NATIVE_MARKDOWN_OWNER.with(|owner| {
            // Parent autosave or result delivery may temporarily lease Active; its modal cell must retain the same provider authority.
            // The address compares an already-live parent with its existing lease; it proves neither liveness nor Closing authority.
            owner.get() == Some(std::ptr::from_ref(parent).addr())
        })
}

/// Keeps weak identities and scalar admission on this stack, without leasing the Active surface across the nested modal loop.
pub(super) struct NativeMarkdownModalUseGuard {
    generation: u64,
    parent: Weak<NativeMarkdownController>,
    pub(super) ledger: Rc<NativeMarkdownOwnedLedger>,
    finished_input: RefCell<Option<NativeMarkdownBorrowedEditorSnapshot>>,
}

impl NativeMarkdownModalUseGuard {
    /// Rejects nested or stale callers before acquiring the current editor/root ledger's shared-backing modal scope.
    pub(super) fn enter(
        controller: &NativeMarkdownController,
    ) -> Result<Self, NativeMarkdownModalError> {
        MainThreadMarker::new().ok_or(NativeMarkdownModalError::StaleOwner)?;
        preflight_native_markdown_modal().map_err(|_| NativeMarkdownModalError::Busy)?;
        let ledger = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
            let surface = active
                .as_ref()
                .ok_or(NativeMarkdownModalError::StaleOwner)?;
            if !std::ptr::eq(&*surface.controller, controller) {
                // A different live controller cannot bind an old callback to the replacement session.
                return Err(NativeMarkdownModalError::StaleOwner);
            }
            surface
                .preflight_modal_boundary()
                .map_err(NativeMarkdownModalError::Boundary)?;
            if surface.pending_note_switch.is_some() || surface.pending_file_mutation.is_some() {
                // An accepted note/file handoff owns its original release boundary and cannot admit a new dialog.
                return Err(NativeMarkdownModalError::Busy);
            }
            let source = surface
                .editor
                .as_ref()
                .map_or(&*surface.owned_ledger, |editor| {
                    // The active note records modal roles in both its local editor ledger and the shared root.
                    editor.owned_ledger()
                });
            Ok(Rc::new(source.modal_scope()))
        })?;
        let generation = claim_modal_generation()?;
        let mut guard = Self {
            generation,
            parent: Weak::default(),
            ledger,
            finished_input: RefCell::new(None),
        };
        // Weak initialization enters the runtime after admission is claimed, so reentry cannot accept parent closure.
        guard.parent = Weak::from(controller);
        if !guard.is_current() {
            // If initialization invalidated the owner, Drop releases only this guard's scalar claim.
            return Err(NativeMarkdownModalError::StaleOwner);
        }
        Ok(guard)
    }

    /// Combines weak liveness with the actual parent owner and this modal epoch.
    fn is_current(&self) -> bool {
        MODAL_ACTIVE.with(Cell::get) == Some(self.generation)
            && self.parent.load().is_some_and(|parent| {
                // The temporary upgraded owner is used only for the pure Active/borrowed identity check.
                modal_parent_is_current(&parent)
            })
    }

    /// Retains cleanup failure on the original parent; ordinary cancellation, Busy, and input-budget rejection do not poison history.
    pub(super) fn record_cleanup_failure(&self, error: &str) {
        let Some(parent) = self.parent.load() else {
            // An expired parent has no writable failure owner; do not transfer its error to a later session.
            return;
        };
        ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow_mut(|active| {
            if let Some(surface) = active.as_mut()
                && std::ptr::eq(&*surface.controller, &*parent)
            {
                // Updates only bounded Rust error text; no native API runs under the surface borrow.
                surface
                    .modal_failure
                    .get_or_insert_with(|| error.to_owned());
            }
        });
    }

    /// Rechecks public weak release facts after the caller's pool ends, without retaining native dialog owners.
    pub(super) fn complete<T>(
        &self,
        result: Result<T, NativeMarkdownModalError>,
    ) -> Result<T, NativeMarkdownModalError> {
        let borrowed = self.finished_input.borrow().clone();
        if let Some(borrowed) = borrowed
            && !borrowed.is_detached()
        {
            // Cleanup inside the pool is insufficient when final public detachment facts still fail; preserve the primary error.
            let error = borrowed
                .failure()
                .unwrap_or_else(|| "对话框输入尚未清空或解除关联".into());
            self.record_cleanup_failure(&error);
            return Err(NativeMarkdownModalError::Boundary(error));
        }
        result
    }
}

impl Drop for NativeMarkdownModalUseGuard {
    fn drop(&mut self) {
        // Ends only the acquired scalar claim; performs no native retain/release, UI, or save.
        release_modal_generation(self.generation);
    }
}

/// Owns one dialog's standard input; native back-references remain weak.
pub(super) struct ModalInputIvars {
    ledger: Rc<NativeMarkdownOwnedLedger>,
    undo: Rc<NativeMarkdownSessionUndo>,
    provider: NativeMarkdownFieldEditorProvider,
    borrowed: NativeMarkdownBorrowedEditorSnapshot,
    parent: Weak<NativeMarkdownController>,
    field: OnceCell<Weak<NSTextField>>,
    window: OnceCell<Weak<NSWindow>>,
    generation: u64,
    active: Cell<bool>,
    failure: RefCell<Option<String>>,
    routing: routing::ModalUndoRoute,
}

define_class!(
    // SAFETY: This main-thread NSResponder implements public text delegation; initialized ivars and the dialog owner keep the chain node alive.
    #[unsafe(super = NSResponder)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ModalInputIvars]
    pub(super) struct NativeMarkdownModalInput;

    // SAFETY: The initialized NSResponder subclass inherits NSObject identity and is used only on the AppKit main thread.
    unsafe impl NSObjectProtocol for NativeMarkdownModalInput {}
    // SAFETY: Notification selector ABIs match the public protocol; cleanup disconnects the field delegate first.
    unsafe impl NSControlTextEditingDelegate for NativeMarkdownModalInput {
        #[unsafe(method(control:textShouldEndEditing:))]
        /// Validates the original client and history before resignation; refusal preserves undo groups, input, and backing.
        fn should_end(&self, control: &NSControl, editor: &NSText) -> bool {
            self.preflight_resignation(control, editor).is_ok()
        }

        #[unsafe(method(controlTextDidBeginEditing:))]
        /// Observes this field's actual editor/history on begin without prematurely ending its undo group.
        fn did_begin(&self, notification: &NSNotification) {
            self.observe(notification, false);
        }
        #[unsafe(method(controlTextDidChange:))]
        /// Rechecks ownership after actual input changes while preserving the focused client's Undo/Redo.
        fn did_change(&self, notification: &NSNotification) {
            self.observe(notification, false);
        }
        #[unsafe(method(controlTextDidEndEditing:))]
        /// Applies the shared focus-history boundary only after native validation emits a successful end notification.
        fn did_end(&self, notification: &NSNotification) {
            // Only successful didEnd may end focus history; provider queries and failed shouldEnd leave it intact.
            self.observe(notification, true);
        }
    }
    // SAFETY: NSTextFieldDelegate inherits these public control-text notifications under the same main-thread lifetime.
    unsafe impl NSTextFieldDelegate for NativeMarkdownModalInput {}
    // SAFETY: The combo retains AppKit suggestion and selection behavior; this class uses only inherited text notifications.
    unsafe impl NSComboBoxDelegate for NativeMarkdownModalInput {}

    // SAFETY: Public validation arguments and BOOL ABI match; validation does not mutate the manager, input, or modal authority.
    unsafe impl NSUserInterfaceValidations for NativeMarkdownModalInput {
        #[unsafe(method(validateUserInterfaceItem:))]
        /// Authorizes only this node's two history actions; other actions retain their existing responder-chain receivers.
        fn validate_item(&self, item: &ProtocolObject<dyn NSValidatedUserInterfaceItem>) -> bool {
            match item.action() {
                // Standard Undo validation queries existing native history without constructing a manager.
                Some(action) if action == sel!(undo:) => routing::can_perform(self, false),
                // Redo independently queries the native redo stack instead of inferring availability from Undo.
                Some(action) if action == sel!(redo:) => routing::can_perform(self, true),
                // This node does not implement other actions and cannot validate them on another responder's behalf.
                _ => false,
            }
        }
    }

    impl NativeMarkdownModalInput {
        #[unsafe(method(undo:))]
        /// Executes only this live input's registered history after standard nil-target routing reaches this node.
        fn undo_input(&self, _sender: Option<&AnyObject>) {
            routing::perform(self, false);
        }

        #[unsafe(method(redo:))]
        /// Replays the real NSTextView's native redo entries without introducing another business history model.
        fn redo_input(&self, _sender: Option<&AnyObject>) {
            routing::perform(self, true);
        }
    }
);

impl NativeMarkdownModalInput {
    /// Creates history and a provider owned by this dialog, separate from the parent query and document history.
    fn new(guard: &NativeMarkdownModalUseGuard, mtm: MainThreadMarker) -> Retained<Self> {
        let undo = Rc::new(NativeMarkdownSessionUndo::default());
        let borrowed = NativeMarkdownBorrowedEditorSnapshot::default();
        let ledger = Rc::clone(&guard.ledger);
        let provider = NativeMarkdownFieldEditorProvider::new(&undo, &ledger, &borrowed);
        let allocated = Self::alloc(mtm).set_ivars(ModalInputIvars {
            ledger,
            undo,
            provider,
            borrowed,
            parent: guard.parent.clone(),
            field: OnceCell::new(),
            window: OnceCell::new(),
            generation: guard.generation,
            active: Cell::new(true),
            failure: RefCell::new(None),
            routing: routing::ModalUndoRoute::default(),
        });
        // SAFETY: The public NSResponder initializer consumes a main-thread subclass allocation with fully initialized ivars.
        unsafe { msg_send![super(allocated), init] }
    }

    /// Binds fixed weak identities once, without native messages under RefCell borrows or replacement field sets.
    fn bind(&self, field: &NSTextField, window: &NSWindow) -> Result<(), NativeMarkdownModalError> {
        self.ivars().field.set(Weak::from(field)).map_err(|_| {
            // Rebinding is not valid modal reuse; reject redirecting the original cell to another client.
            NativeMarkdownModalError::Boundary("对话框输入被重复关联".into())
        })?;
        self.ivars().window.set(Weak::from(window)).map_err(|_| {
            // Window identity is installed once during construction and cannot revive a finished provider.
            NativeMarkdownModalError::Boundary("对话框窗口被重复关联".into())
        })?;
        // SAFETY: The retained input owner keeps this weak delegate alive; the public protocol matches and finish disconnects it first.
        unsafe { field.setDelegate(Some(ProtocolObject::from_ref(self))) };
        Ok(())
    }

    /// Requires the actual parent and matching epoch; a stored address or active flag alone cannot prove liveness.
    fn is_current(&self) -> bool {
        self.ivars().active.get()
            && MODAL_ACTIVE.with(Cell::get) == Some(self.ivars().generation)
            && self.ivars().parent.load().is_some_and(|parent| {
                // The temporary upgraded claim covers only pure identity checks, without mutably borrowing the parent surface.
                modal_parent_is_current(&parent)
            })
    }

    /// Supplies this dialog's lazy editor only through the public cell extension for the exact field and window.
    fn provide(&self, control: &NSView) -> Option<Retained<NSTextView>> {
        if !self.is_current() {
            // A late cell query allocates no editor/history and does not take over an unknown framework client.
            return None;
        }
        let field = self.ivars().field.get()?.load()?;
        let window = self.ivars().window.get()?.load()?;
        let expected_view: &NSView = &field;
        if !std::ptr::eq(expected_view, control)
            || !control.window().is_some_and(|actual| {
                // Both windows are live owners; the parent panel or another alert cannot use this provider.
                std::ptr::eq(&*actual, &*window)
            })
        {
            // An unmatched controlView remains the responsibility of its own standard AppKit path.
            return None;
        }
        let editor = self.ivars().provider.provide(Some(&field), self.mtm())?;
        match editor.downcast::<NSTextView>() {
            Ok(editor) if self.is_current() => {
                // The provider checks closed/creating state and ledger ownership before returning the actual standard field editor.
                Some(editor)
            }
            Ok(_) => {
                // If provider reentry closed the dialog, drop the local claim without publishing it.
                None
            }
            Err(_) => {
                // The concrete provider promises NSTextView; retain a type mismatch as the primary failure rather than accepting an unknown editor.
                self.record_failure("对话框输入控件类型无效".into());
                None
            }
        }
    }

    /// Observes history or successful focus completion only for this bound field under the current owner.
    fn observe(&self, notification: &NSNotification, did_end: bool) {
        if !self.is_current() {
            // Late notifications after accepted cleanup cannot restore authority or clear a later session's history.
            return;
        }
        let Some(field) = notification
            .object()
            .and_then(|object| object.downcast::<NSTextField>().ok())
        else {
            // A foreign notification grants no inferred text-field identity.
            return;
        };
        if !self
            .ivars()
            .field
            .get()
            .and_then(Weak::load)
            .is_some_and(|expected| {
                // Only the actual client bound to this dialog may advance its history boundary.
                std::ptr::eq(&*expected, &*field)
            })
        {
            return;
        }
        let result = if did_end {
            // Reuses the shared focus policy instead of duplicating Undo coalescing or clearing.
            self.ivars().borrowed.finished_control_editing(
                &field,
                &self.ivars().undo,
                &self.ivars().ledger,
            )
        } else {
            // Begin/change verifies the actual editor and manager without clearing this client's usable history.
            self.ivars()
                .borrowed
                .observe_editing(&field, &self.ivars().undo, &self.ivars().ledger)
        };
        if let Err(error) = result {
            // The delegate has no Result channel; the explicit response path propagates this failure before allowing file operations.
            self.record_failure(error);
        } else if !did_end {
            // Installs routing only after the real editor is present; successful didEnd must not reconnect a finished input.
            routing::install_after_editing(self);
        }
    }

    /// Records the dialog's first failure when a delegate cannot return Result; explicit completion owns propagation.
    fn record_failure(&self, error: String) {
        // The bounded error owns no document, field, or manager and never overwrites an earlier ownership failure.
        self.ivars().failure.borrow_mut().get_or_insert(error);
    }

    /// Implements AppKit's public refusal boundary; observes the live client without cleanup reserved for successful didEnd.
    fn preflight_resignation(
        &self,
        control: &NSControl,
        editor: &NSText,
    ) -> Result<(), NativeMarkdownModalError> {
        if !self.is_current() {
            // A finished or stale provider cannot authorize another window's resignation through a late callback.
            return Err(NativeMarkdownModalError::StaleOwner);
        }
        let field = self
            .ivars()
            .field
            .get()
            .and_then(Weak::load)
            .ok_or(NativeMarkdownModalError::StaleOwner)?;
        let expected_control: &NSControl = &field;
        if !std::ptr::eq(expected_control, control)
            || !field
                .currentEditor()
                .is_some_and(|actual| std::ptr::eq(&*actual, editor))
        {
            // Rejects another control/editor's validation request while preserving its unknown history.
            return Err(NativeMarkdownModalError::Boundary(
                "对话框输入的结束请求与当前控件不匹配".into(),
            ));
        }
        self.ivars()
            .undo
            .preflight()
            .map_err(NativeMarkdownModalError::Boundary)?;
        self.ivars()
            .borrowed
            .observe_editing(&field, &self.ivars().undo, &self.ivars().ledger)
            .map_err(NativeMarkdownModalError::Boundary)?;
        self.ivars()
            .borrowed
            .preflight_unmarked()
            .map_err(NativeMarkdownModalError::Boundary)?;
        if let Some(error) = self.ivars().failure.borrow().clone() {
            // Focus movement cannot erase a known ownership failure; returning NO preserves the field and its history.
            return Err(NativeMarkdownModalError::Boundary(error));
        }
        Ok(())
    }

    /// Runs normal validation and resignation before clearing or reading a confirmed candidate.
    pub(super) fn prepare_end(&self, window: &NSWindow) -> Result<(), NativeMarkdownModalError> {
        self.ivars()
            .undo
            .preflight()
            .map_err(NativeMarkdownModalError::Boundary)?;
        self.ivars()
            .borrowed
            .capture(window)
            .map_err(NativeMarkdownModalError::Boundary)?;
        if let Some(error) = self.ivars().failure.borrow().clone() {
            // Rejects a recorded failure before resignation, so successful didEnd cannot clear history that must remain recoverable.
            return Err(NativeMarkdownModalError::Boundary(error));
        }
        if !window.makeFirstResponder(None) {
            // A refusal cannot be converted to success by forced endEditing; no file effect has reached the parent draft.
            return Err(NativeMarkdownModalError::Boundary(
                "对话框输入尚未结束，请完成输入后重试".into(),
            ));
        }
        self.ivars()
            .borrowed
            .preflight_unmarked()
            .map_err(NativeMarkdownModalError::Boundary)?;
        if let Some(error) = self.ivars().failure.borrow().clone() {
            // Preserves ownership failures from actual begin/change/end notifications, including on cancellation.
            return Err(NativeMarkdownModalError::Boundary(error));
        }
        Ok(())
    }

    /// Ends authority only after normal resignation; candidate-read failure still permits cleanup of this accepted owner.
    pub(super) fn finish(
        &self,
        window: &NSWindow,
        field: &NSTextField,
    ) -> Result<(), NativeMarkdownModalError> {
        self.ivars().active.set(false);
        // Authority is sealed first; removes this route before provider/input cleanup can reenter through native messages.
        routing::detach(self);
        self.ivars().provider.freeze();
        self.ivars().borrowed.finish_accepted(
            window,
            &[field],
            &self.ivars().undo,
            &self.ivars().provider,
            true,
        );
        field.setStringValue(ns_string!(""));
        field.setPlaceholderString(None);
        field.setEditable(false);
        field.setSelectable(false);
        // SAFETY: Removes nullable callbacks only on this dialog's accessory; NSAlert reserved button actions remain untouched.
        unsafe {
            field.setDelegate(None);
            field.setTarget(None);
            field.setAction(None);
        }
        if !self.ivars().borrowed.is_detached() {
            // Nominal cleanup is insufficient: failed public detachment blocks successors and cannot be relabeled as prepared input.
            return Err(NativeMarkdownModalError::Boundary(
                self.ivars()
                    .borrowed
                    .failure()
                    .unwrap_or_else(|| "对话框输入尚未清空或解除关联".into()),
            ));
        }
        if let Some(error) = self.ivars().failure.borrow().clone() {
            // Retains the native delegate's first failure even if later public empty-state checks succeed.
            return Err(NativeMarkdownModalError::Boundary(error));
        }
        Ok(())
    }
}

/// Cells weakly reference their isolated input owner while preserving AppKit text and IME behavior.
struct ModalCellIvars {
    owner: Weak<NativeMarkdownModalInput>,
}

define_class!(
    // SAFETY: Preserves NSTextFieldCell behavior, overriding only public fieldEditorForView with ivars initialized before super init.
    #[unsafe(super = NSTextFieldCell)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ModalCellIvars]
    struct NativeMarkdownModalNameCell;
    impl NativeMarkdownModalNameCell {
        #[unsafe(method_id(fieldEditorForView:))]
        fn field_editor(&self, view: &NSView) -> Option<Retained<NSTextView>> {
            // The weak upgrade covers one synchronous query; the cell cannot keep the dialog or parent session alive.
            self.ivars().owner.load().and_then(|owner| {
                // method_id preserves the native Option return ABI; an absent owner grants no input authority.
                owner.provide(view)
            })
        }
    }
);

define_class!(
    // SAFETY: Preserves NSComboBoxCell suggestions and editing, replacing only the public field-editor provider entry.
    #[unsafe(super = NSComboBoxCell)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ModalCellIvars]
    struct NativeMarkdownModalMoveCell;
    impl NativeMarkdownModalMoveCell {
        #[unsafe(method_id(fieldEditorForView:))]
        fn field_editor(&self, view: &NSView) -> Option<Retained<NSTextView>> {
            // The isolated owner verifies control, window, and epoch; other combo clients receive no authority.
            self.ivars().owner.load().and_then(|owner| {
                // Only a live dialog owner supplies the editor; a failed weak upgrade allocates no replacement.
                owner.provide(view)
            })
        }
    }
);

/// Creates and records the owned name cell before candidate publication or modal event processing.
pub(super) fn install_native_markdown_modal_name_input(
    guard: &NativeMarkdownModalUseGuard,
    input: &NSTextField,
    window: &NSWindow,
    mtm: MainThreadMarker,
) -> Result<Retained<NativeMarkdownModalInput>, NativeMarkdownModalError> {
    let owner = NativeMarkdownModalInput::new(guard, mtm);
    let allocated = NativeMarkdownModalNameCell::alloc(mtm).set_ivars(ModalCellIvars {
        owner: Weak::from_retained(&owner),
    });
    // SAFETY: The main-thread NSTextFieldCell subclass has an initialized weak ivar and uses the public text initializer with NSString.
    let cell: Retained<NativeMarkdownModalNameCell> =
        unsafe { msg_send![super(allocated), initTextCell: ns_string!("")] };
    input.setCell(Some(&cell));
    guard
        .ledger
        .include("modal.input-controller", &owner)
        .map_err(NativeMarkdownModalError::Boundary)?;
    guard
        .ledger
        .include_control("modal.name-input", input)
        .map_err(NativeMarkdownModalError::Boundary)?;
    owner.bind(input, window)?;
    Ok(owner)
}

/// Uses an owned standard combo cell for Move, preserving the NSAlert delegate and private controls.
pub(super) fn install_native_markdown_modal_move_input(
    guard: &NativeMarkdownModalUseGuard,
    input: &objc2_app_kit::NSComboBox,
    window: &NSWindow,
    mtm: MainThreadMarker,
) -> Result<Retained<NativeMarkdownModalInput>, NativeMarkdownModalError> {
    let owner = NativeMarkdownModalInput::new(guard, mtm);
    let allocated = NativeMarkdownModalMoveCell::alloc(mtm).set_ivars(ModalCellIvars {
        owner: Weak::from_retained(&owner),
    });
    // SAFETY: Public NSComboBoxCell initTextCell consumes the correct subclass allocation with its weak ivar initialized.
    let cell: Retained<NativeMarkdownModalMoveCell> =
        unsafe { msg_send![super(allocated), initTextCell: ns_string!("")] };
    input.setCell(Some(&cell));
    guard
        .ledger
        .include("modal.input-controller", &owner)
        .map_err(NativeMarkdownModalError::Boundary)?;
    guard
        .ledger
        .include_control("modal.move-input", input)
        .map_err(NativeMarkdownModalError::Boundary)?;
    owner.bind(input, window)?;
    Ok(owner)
}

/// Records the alert, actual window, and configured buttons/cells without traversing private views or clearing reserved actions.
pub(super) fn register_native_markdown_modal_alert(
    guard: &NativeMarkdownModalUseGuard,
    alert: &NSAlert,
) -> Result<Retained<NSWindow>, NativeMarkdownModalError> {
    guard
        .ledger
        .include("modal.alert", alert)
        .map_err(NativeMarkdownModalError::Boundary)?;
    let window = alert.window();
    guard
        .ledger
        .include("modal.window", &window)
        .map_err(NativeMarkdownModalError::Boundary)?;
    let buttons = alert.buttons();
    if buttons.count() > 3 {
        // The project creates at most three response buttons; an unexpected count must not result in partial recording and presentation.
        return Err(NativeMarkdownModalError::Boundary(
            "对话框操作按钮超过数量上限".into(),
        ));
    }
    for index in 0..buttons.count() {
        // The returned buttons and actual cells are configured roles; their target/action remain entirely owned by NSAlert.
        let button = buttons.objectAtIndex(index);
        guard
            .ledger
            .include_control("modal.response-button", &button)
            .map_err(NativeMarkdownModalError::Boundary)?;
    }
    Ok(window)
}

/// Clears only project-owned public customization, preserving NSAlert reserved button actions and its private window delegate.
pub(super) fn detach_native_markdown_modal_alert(alert: &NSAlert, window: &NSWindow) {
    window.setInitialFirstResponder(None);
    alert.setAccessoryView(None);
    alert.setMessageText(ns_string!(""));
    alert.setInformativeText(ns_string!(""));
    let buttons = alert.buttons();
    for index in 0..buttons.count() {
        // Button title is a public customization point; removes business text without touching reserved callbacks.
        // All buttons were recorded before presentation within the three-button bound; no buttons are added during this modal.
        let button = buttons.objectAtIndex(index);
        button.setTitle(ns_string!(""));
    }
}

/// Shares explicit recording and pool boundaries for confirmation/information alerts without hiding business actions.
pub(super) fn run_native_markdown_modal_confirmation(
    controller: &NativeMarkdownController,
    message: &str,
    information: &str,
    action: &str,
    cancel: Option<&str>,
    mtm: MainThreadMarker,
) -> Result<bool, NativeMarkdownModalError> {
    let guard = NativeMarkdownModalUseGuard::enter(controller)?;
    let result = autoreleasepool(|_| {
        let alert = NSAlert::new(mtm);
        alert.addButtonWithTitle(&NSString::from_str(action));
        if let Some(cancel) = cancel {
            // Assigns Escape only to an explicit Cancel button, preserving the framework's reserved target/action.
            assign_escape_to_native_markdown_alert_cancel(
                &alert.addButtonWithTitle(&NSString::from_str(cancel)),
            );
        }
        let window = register_native_markdown_modal_alert(&guard, &alert)?;
        alert.setMessageText(&NSString::from_str(message));
        alert.setInformativeText(&NSString::from_str(information));
        #[cfg(all(
            feature = "native-markdown-runtime-probe",
            feature = "native-table-row-append"
        ))]
        NativeMarkdownHostedTableProbe::observe_conflict_modal_ready(&alert, &window, None);
        let confirmed = native_markdown_alert_response(alert.runModal(), cancel.is_some());
        detach_native_markdown_modal_alert(&alert, &window);
        confirmed
    });
    guard.complete(result)
}

/// Returns a bounded owned candidate; oversized native input or UTF-8 output is rejected explicitly.
pub(super) fn native_markdown_modal_candidate(
    input: &NSTextField,
) -> Result<String, NativeMarkdownModalError> {
    autoreleasepool(|pool| {
        let value = input.stringValue();
        if value.length() > MAX_MODAL_INPUT_UNITS {
            // Rejects before creating an unbounded Rust String; truncating a path or title could change the file operation.
            return Err(NativeMarkdownModalError::InputTooLong);
        }
        // SAFETY: The innermost pool and value cover synchronous length/trim/copy, with no native setter or external callback.
        let value = unsafe { value.to_str(pool) };
        if value.len() > MAX_MODAL_INPUT_BYTES {
            // The UTF-16 bound does not establish the UTF-8 byte budget; both are checked independently.
            return Err(NativeMarkdownModalError::InputTooLong);
        }
        Ok(value.trim().to_owned())
    })
}

/// Preserves failed input in the same alert until normal resignation succeeds after a subsequent explicit user action.
pub(super) fn run_native_markdown_modal_input_response(
    alert: &NSAlert,
    window: &NSWindow,
    field: &NSTextField,
    owner: &NativeMarkdownModalInput,
) -> Result<bool, NativeMarkdownModalError> {
    #[cfg(all(
        feature = "native-markdown-runtime-probe",
        feature = "native-table-row-append"
    ))]
    NativeMarkdownHostedTableProbe::observe_conflict_modal_ready(alert, window, Some(field));
    await_prepared_modal_response(
        || {
            // The same owner stays on the outer stack; each action drains framework temporaries in a local pool instead of accumulating them.
            autoreleasepool(|_| native_markdown_alert_response(alert.runModal(), true))
        },
        || autoreleasepool(|_| owner.prepare_end(window)),
        |_| {
            autoreleasepool(|_| {
                // runModal does not guarantee accessory resignation; the original alert, field, editor, and undo remain on this stack.
                // Updates fixed feedback and public initial focus only; does not read candidates, clear input, freeze the provider, or create a transaction.
                let message = if owner.ivars().undo.has_failure()
                    || owner.ivars().borrowed.failure().is_some()
                    || owner.ivars().failure.borrow().is_some()
                {
                    // Another Cancel cannot repair a sticky ownership failure; retains the failed resource state without claiming automatic recovery.
                    ns_string!("当前输入无法安全结束，内容将保留在此窗口中，本次操作尚未执行。")
                } else {
                    // An unfinished composition or refused resignation preserves input for validation after the next user action.
                    ns_string!("请完成当前输入后重试。文字与撤销记录均已保留，本次操作尚未执行。")
                };
                alert.setInformativeText(message);
                window.setInitialFirstResponder(Some(field));
                let _ = window.makeFirstResponder(Some(field));
            });
        },
    )
}

/// Waits for another explicit interaction after failure, without polling, timers, recursion, or replacing the dialog owner.
fn await_prepared_modal_response<R, E>(
    mut present: impl FnMut() -> R,
    mut prepare: impl FnMut() -> Result<(), E>,
    mut restore: impl FnMut(&E),
) -> R {
    loop {
        // The real modal loop blocks present; an earlier button response cannot bypass failed resignation to obtain file authority.
        let response = present();
        match prepare() {
            Ok(()) => {
                // Publishes the original typed response only after normal resignation; candidate acquisition and cleanup then run once.
                return response;
            }
            Err(error) => {
                // Preserves the same editable input and history; neither relabels failure as Cancel nor performs accepted cleanup.
                restore(&error);
            }
        }
    }
}

/// Finalizes every normally resigned result; candidate errors do not bypass cleanup of the accepted owner.
pub(super) fn finish_native_markdown_modal_input(
    guard: &NativeMarkdownModalUseGuard,
    owner: &NativeMarkdownModalInput,
    window: &NSWindow,
    field: &NSTextField,
) -> Result<(), NativeMarkdownModalError> {
    let result = owner.finish(window, field);
    // Carries only the pure Rust tracker for post-pool checks, without exporting strong editor or manager owners.
    *guard.finished_input.borrow_mut() = Some(owner.ivars().borrowed.clone());
    if let Err(error) = &result {
        // The isolated input's first cleanup failure participates in later parent preflight; cancellation cannot erase it.
        guard.record_cleanup_failure(&error.to_string());
    }
    result
}

/// Treats file-chooser Cancel as a normal no-op; Abort and unknown responses remain failures.
pub(super) fn native_markdown_open_panel_response(
    response: isize,
) -> Result<bool, NativeMarkdownModalError> {
    match response {
        1 => {
            // NSModalResponseOK confirms selection; the caller must still validate the actual file URL/path.
            Ok(true)
        }
        0 => {
            // NSModalResponseCancel grants no candidate or file effect.
            Ok(false)
        }
        -1001 => {
            // NSModalResponseAbort denotes system presentation failure rather than user cancellation.
            Err(NativeMarkdownModalError::Aborted)
        }
        other => {
            // An unknown response grants no selection authority; retains its bounded numeric value for diagnosis.
            Err(NativeMarkdownModalError::UnexpectedResponse(other))
        }
    }
}

/// Project alerts add explicit buttons; only an actually installed second button can mean Cancel.
pub(super) fn native_markdown_alert_response(
    response: isize,
    has_cancel: bool,
) -> Result<bool, NativeMarkdownModalError> {
    match response {
        1000 => {
            // NSAlertFirstButtonReturn is this alert's explicit confirmation, independent of visual button order.
            Ok(true)
        }
        1001 if has_cancel => {
            // Only the installed second Cancel response is a normal no-op.
            Ok(false)
        }
        -1001 => {
            // System abort is not user cancellation and cannot grant successor authority or hide presentation failure.
            Err(NativeMarkdownModalError::Aborted)
        }
        other => {
            // A missing Cancel button, unknown extra button, or stop response cannot be relabeled as normal confirmation.
            Err(NativeMarkdownModalError::UnexpectedResponse(other))
        }
    }
}
