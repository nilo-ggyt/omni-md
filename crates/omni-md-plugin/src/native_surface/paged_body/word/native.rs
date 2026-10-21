//! Original keyDown waits for actual navigation receipts, then the sole NSTextInputContext interprets it synchronously without duplicate marked state.

use super::*;
use crate::native_surface::paged_input::NativeMarkdownPagedInputState;
use objc2::rc::Retained;
use objc2::runtime::Sel;
use objc2_app_kit::NSEvent;

pub(in crate::native_surface::paged_body) enum NativeInput {
    Key {
        event: Retained<NSEvent>,
        bytes: usize,
    },
    Command(Sel),
    Paste(String),
    Attachments {
        paths: Vec<std::path::PathBuf>,
        /// Retain only the visible hit's canonical generation/position; text changes cannot redirect an old drop into a successor document.
        drop: Option<(u64, usize)>,
    },
    Format(omni_markdown_core::MarkdownFormatAction),
    InsertTable,
    Fold(super::super::super::paged_text_view::NativeMarkdownPagedSourceFoldAction),
}
impl NativeInput {
    /// Event count also obeys the existing 32-run bound; character payload shares the original byte allowance with ordinary insertion.
    pub(in crate::native_surface::paged_body) fn bytes(&self) -> usize {
        match self {
            Self::Key { bytes, .. } => *bytes,
            Self::Command(_) | Self::Format(_) | Self::InsertTable | Self::Fold(_) => 0,
            Self::Paste(text) => text.len(),
            Self::Attachments { paths, .. } => {
                paths.iter().map(|path| path.as_os_str().len()).sum()
            }
        }
    }
}

impl NativeMarkdownPagedBody {
    /// Formatting/table insertion retains domain intent only; actual selection comes from the preceding Commit when this action reaches queue head.
    pub(in crate::native_surface::paged_body) fn accept_native_edit(
        &self,
        action: NativeInput,
        permit: &NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        let files = matches!(&action, NativeInput::Attachments { .. });
        if let Some(producer) = self.producer.borrow().as_ref() {
            producer.target_source_health()?;
        }
        if let NativeInput::Attachments { paths, .. } = &action {
            crate::native_surface::workspace_actions::admit_attachment_paths(0, paths)
                .map_err(ViewError::AttachmentImport)?; // Reject an invalid external batch as a whole before creating any input barrier.
        }
        let Some(pending) = self.native_input_chain(files)? else {
            return Ok(false);
        };
        // External menus arriving during interpretation stay at the original queue tail; View explicitly bypasses defer for this head action itself.
        let slot = Rc::new(RefCell::new(None));
        {
            let mut steps = pending.steps.borrow_mut();
            if steps.len() >= 32 {
                return Err(ViewError::Busy.into());
            }
            if let NativeInput::Attachments { paths, .. } = &action {
                // Match the existing file entry's four-pending-batch bound without truncating an already-admitted path prefix.
                let queued = steps.iter().filter(|step| matches!(step,
                    WordInput::Native(slot) if matches!(&*slot.borrow(), Some(NativeInput::Attachments { .. }))
                )).count();
                crate::native_surface::workspace_actions::admit_attachment_paths(queued, paths)
                    .map_err(ViewError::AttachmentImport)?;
            }
            let bytes = action.bytes();
            // Reserve bounded payload ownership before publishing the action into
            // its slot; a failed original permit check returns exactly this charge.
            if bytes
                > crate::native_surface::text_storage::NATIVE_MARKDOWN_INPUT_TRANSACTION_MAX_BYTES
                    .saturating_sub(pending.queued_bytes.get())
            {
                return Err(ViewError::Busy.into());
            }
            pending.queued_bytes.set(pending.queued_bytes.get() + bytes);
            steps.push_back(WordInput::Native(slot.clone()));
        }
        let serial = self.serial.get();
        let current = |epoch| self.current(epoch) && pending.current_native_at(self, serial);
        let checked = self
            .input
            .check_permit(permit, &current)
            .map_err(ViewError::from);
        if let Err(cause) = checked {
            pending
                .queued_bytes
                .set(pending.queued_bytes.get() - action.bytes());
            pending.steps.borrow_mut().retain(
                |step| !matches!(step, WordInput::Native(actual) if Rc::ptr_eq(actual, &slot)),
            );
            if files
                && pending.steps.borrow().is_empty()
                && pending.waiting_vertical.borrow().is_none()
            {
                self.cancel_original_word_delete(&pending)?; // Return only this request's empty chain; a rejected batch cannot leave a permanent input barrier.
            } else if pending.current_at(self, serial) {
                self.schedule()?; // Getter reentry may have consumed the wake; resume only the original chain, without replaying the rejected action.
            }
            return Err(cause.into());
        }
        *slot.borrow_mut() = Some(action); // Original Native validation returned; filling this reserved slot removes its incomplete-payload barrier.
        self.schedule()?;
        Ok(true)
    }

    /// Ordinary keys defer only behind existing Word/vertical barriers. File I/O yields the Native callback and runs later in this same chain.
    fn native_input_chain(&self, for_files: bool) -> Result<Option<Rc<PendingWordDelete>>> {
        if let Some(pending) = self.pending_word_delete.borrow().clone() {
            return Ok(Some(pending));
        }
        let vertical = self.pending_vertical.borrow().clone();
        if vertical.is_none() && !for_files {
            return Ok(None);
        }
        if vertical
            .as_ref()
            .is_some_and(|vertical| !vertical.current_native(self))
        {
            return Err(ViewError::Revoked.into());
        }
        let request = self.view()?.capture_scene_request()?;
        if vertical.as_ref().is_some_and(|vertical| {
            !vertical.current_native(self) || !vertical.matches(self, &request)
        }) {
            return Err(ViewError::Revoked.into());
        }
        let current = |epoch| {
            self.current(epoch)
                && vertical
                    .as_ref()
                    .is_none_or(|vertical| vertical.current_native(self))
        };
        let permit = self
            .input
            .capture(self.configuration.epoch, &current)
            .map_err(ViewError::from)?;
        let pending = Rc::new(PendingWordDelete {
            // This is the same production input chain, initially waiting on the
            // actual vertical owner or yielding file work out of its Native callback.
            request: RefCell::new(request),
            steps: RefCell::new(VecDeque::new()),
            queued_bytes: Cell::new(0),
            receipt: RefCell::new(None),
            retired_serial: Cell::new(None),
            permit: RefCell::new(Some(permit)),
            bound: RefCell::new(None),
            ready: RefCell::new(None),
            fold_expansion: RefCell::new(None),
            fold_failure: RefCell::new(None),
            move_prepared: Cell::new(false),
            visual_word: Cell::new(false),
            visual_seed: Cell::new(None),
            visual_ready: Cell::new(None),
            interpreting_native: Cell::new(false),
            native_insert_at: Cell::new(0),
            attachment_failure: RefCell::new(None),
            waiting_vertical: RefCell::new(vertical),
        });
        *self.pending_word_delete.borrow_mut() = Some(Rc::clone(&pending));
        Ok(Some(pending))
    }

    /// Reserve arrival order before getters so reentrant input stays later; conversion failure removes only its own empty slot.
    pub(in crate::native_surface::paged_body) fn accept_native_key(
        &self,
        event: &NSEvent,
        permit: &NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        let Some(pending) = self.native_input_chain(false)? else {
            return Ok(false);
        };
        if let Some(producer) = self.producer.borrow().as_ref() {
            producer.target_source_health()?; // Quota rejection or successor events cannot hide the original Source's actual first failure.
        }
        let slot = Rc::new(RefCell::new(None));
        {
            let mut steps = pending.steps.borrow_mut();
            if steps.len() >= 32 {
                return Err(ViewError::Busy.into());
            }
            steps.push_back(WordInput::Native(Rc::clone(&slot)));
        }
        let serial = self.serial.get();
        let current = |epoch| self.current(epoch) && pending.current_native_at(self, serial);
        let charged = Cell::new(0);
        let copied: Result<NativeInput> = (|| {
            self.input
                .check_permit(permit, &current)
                .map_err(ViewError::from)?;
            let characters = event.characters();
            self.input
                .check_permit(permit, &current)
                .map_err(ViewError::from)?;
            let plain = event.charactersIgnoringModifiers();
            self.input
                .check_permit(permit, &current)
                .map_err(ViewError::from)?;
            let character_units = characters.as_ref().map_or(0, |text| text.length());
            self.input
                .check_permit(permit, &current)
                .map_err(ViewError::from)?;
            let plain_units = plain.as_ref().map_or(0, |text| text.length());
            self.input
                .check_permit(permit, &current)
                .map_err(ViewError::from)?;
            let units = character_units
                .checked_add(plain_units)
                .ok_or(ViewError::CounterExhausted)?;
            let bytes = units.checked_mul(2).ok_or(ViewError::CounterExhausted)?;
            let maximum =
                crate::native_surface::text_storage::NATIVE_MARKDOWN_INPUT_TRANSACTION_MAX_BYTES;
            if bytes > maximum.saturating_sub(pending.queued_bytes.get()) {
                return Err(ViewError::Busy.into());
            }
            pending.queued_bytes.set(pending.queued_bytes.get() + bytes);
            charged.set(bytes); // Retain and temporary NSString release may reenter; successor input must already see this event's charged allowance.
            // SAFETY: The original synchronous main-thread argument is live; retain keeps that same event without recreating timestamps or key payload.
            let retained = unsafe { Retained::retain(std::ptr::from_ref(event).cast_mut()) }
                .ok_or(ViewError::Revoked)?;
            drop(characters);
            drop(plain); // Temporary NSString release may reenter; install the event only after final original-ticket validation.
            self.input
                .check_permit(permit, &current)
                .map_err(ViewError::from)?;
            Ok(NativeInput::Key {
                event: retained,
                bytes,
            })
        })();
        match copied {
            Ok(input) => {
                *slot.borrow_mut() = Some(input);
                self.schedule()?;
                Ok(true)
            }
            Err(cause) => {
                pending
                    .queued_bytes
                    .set(pending.queued_bytes.get() - charged.get());
                pending.steps.borrow_mut().retain(
                    |step| !matches!(step, WordInput::Native(actual) if Rc::ptr_eq(actual, &slot)),
                );
                if pending.current_at(self, serial) {
                    self.schedule()?; // Payload conversion ended; this same chain must resume work suspended by its original None slot.
                }
                Err(cause)
            }
        }
    }

    /// Menus reserve their original order. Paste converts under cumulative allowance now, without later clipboard rereads or over-budget copies.
    pub(in crate::native_surface::paged_body) fn accept_native_command(
        &self,
        selector: Sel,
        permit: &NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        let Some(pending) = self.native_input_chain(false)? else {
            return Ok(false);
        };
        if let Some(producer) = self.producer.borrow().as_ref() {
            producer.target_source_health()?; // Quota rejection or successor events cannot hide the original Source's actual first failure.
        }
        let slot = Rc::new(RefCell::new(None));
        {
            let mut steps = pending.steps.borrow_mut();
            if steps.len() >= 32 {
                return Err(ViewError::Busy.into());
            }
            steps.push_back(WordInput::Native(Rc::clone(&slot)));
        }
        let serial = self.serial.get();
        let current = |epoch| self.current(epoch) && pending.current_native_at(self, serial);
        let charged = Cell::new(0);
        let captured: Result<NativeInput> = (|| {
            self.input
                .check_permit(permit, &current)
                .map_err(ViewError::from)?;
            if selector != objc2::sel!(paste:) && selector != objc2::sel!(pasteAsPlainText:) {
                return Ok(NativeInput::Command(selector));
            }
            let board = objc2_app_kit::NSPasteboard::generalPasteboard();
            self.input
                .check_permit(permit, &current)
                .map_err(ViewError::from)?;
            if selector == objc2::sel!(paste:) {
                // Files precede text representation. Capture paths at original menu arrival; queue-head execution does not reread a changed clipboard.
                let paths = crate::native_surface::workspace_actions::native_markdown_pasteboard_file_paths(&board)
                    .map_err(ViewError::AttachmentImport)?;
                self.input
                    .check_permit(permit, &current)
                    .map_err(ViewError::from)?;
                if !paths.is_empty() {
                    let queued = pending.steps.borrow().iter().filter(|step| matches!(step,
                        WordInput::Native(slot) if matches!(&*slot.borrow(), Some(NativeInput::Attachments { .. }))
                    )).count();
                    crate::native_surface::workspace_actions::admit_attachment_paths(
                        queued, &paths,
                    )
                    .map_err(ViewError::AttachmentImport)?;
                    let input = NativeInput::Attachments { paths, drop: None };
                    let bytes = input.bytes();
                    if bytes > crate::native_surface::text_storage::NATIVE_MARKDOWN_INPUT_TRANSACTION_MAX_BYTES.saturating_sub(pending.queued_bytes.get()) {
                        return Err(ViewError::Busy.into());
                    }
                    pending.queued_bytes.set(pending.queued_bytes.get() + bytes);
                    charged.set(bytes);
                    return Ok(input);
                }
            }
            // SAFETY: The public string-type constant is used only for this actual menu arrival's clipboard read.
            let text = board.stringForType(unsafe { objc2_app_kit::NSPasteboardTypeString });
            self.input
                .check_permit(permit, &current)
                .map_err(ViewError::from)?;
            let text = text.ok_or(ViewError::ClipboardUnavailable)?;
            let copied = self.input.copy_permit_text_with_limit(permit, &text, &current, &|bytes| {
                // Charge before the real string copy, while preserving any quota
                // already reserved by earlier or reentrant admitted input.
                let remaining = crate::native_surface::text_storage::NATIVE_MARKDOWN_INPUT_TRANSACTION_MAX_BYTES.saturating_sub(pending.queued_bytes.get());
                if bytes > remaining { return false; }
                pending.queued_bytes.set(pending.queued_bytes.get() + bytes);
                charged.set(bytes);
                true
            });
            drop(text);
            drop(board);
            let copied = copied.map_err(ViewError::from)?;
            self.input
                .check_permit(permit, &current)
                .map_err(ViewError::from)?;
            Ok(NativeInput::Paste(copied))
        })();
        match captured {
            Ok(input) => {
                *slot.borrow_mut() = Some(input);
                self.schedule()?;
                Ok(true)
            }
            Err(cause) => {
                pending
                    .queued_bytes
                    .set(pending.queued_bytes.get() - charged.get());
                pending.steps.borrow_mut().retain(
                    |step| !matches!(step, WordInput::Native(actual) if Rc::ptr_eq(actual, &slot)),
                );
                if pending.current_at(self, serial) {
                    self.schedule()?; // Payload conversion ended; this same chain must resume work suspended by its original None slot.
                }
                Err(cause)
            }
        }
    }

    /// Only synchronous input callbacks on the actual interpretation stack may hand off this Commit; external clicks/Find have no such authority.
    pub(in crate::native_surface::paged_body) fn begin_native_commit(
        &self,
        before: NativeMarkdownPagedInputState,
        commit: Rc<NativeMarkdownPagedInputCommit>,
    ) -> Result<bool> {
        let Some(pending) = self.pending_word_delete.borrow().clone() else {
            return Ok(false);
        };
        if !pending.interpreting_native.get() {
            return Ok(false);
        }
        let serial = self.serial.get();
        let mut expected = pending.request();
        if expected.input != before || !pending.current_at_before_commit(self, serial, before) {
            return Err(ViewError::Revoked.into());
        }
        commit
            .check_current(&|epoch| self.current(epoch))
            .map_err(ViewError::from)?;
        expected.input = commit.state;
        let actual = self.view()?.capture_scene_request()?;
        commit
            .check_current(&|epoch| self.current(epoch))
            .map_err(ViewError::from)?;
        if !actual.same(&expected) {
            return Err(ViewError::Revoked.into());
        }
        *pending.receipt.borrow_mut() = Some(commit);
        Ok(true)
    }

    /// The same Commit authorizes owned gutter changes and the next permit; successor Input cannot renew an old event or external resize.
    pub(in crate::native_surface::paged_body) fn finish_native_commit(
        &self,
        commit: &NativeMarkdownPagedInputCommit,
        reveal: bool,
    ) -> Result<()> {
        let pending = self
            .pending_word_delete
            .borrow()
            .clone()
            .ok_or(ViewError::Revoked)?;
        if !pending.interpreting_native.get() || !self.owns_word_commit(commit) {
            return Err(ViewError::Revoked.into());
        }
        let before = pending.request();
        let request = self.synchronize_committed_gutter(commit, &before)?;
        if !pending.current_at(self, self.serial.get()) {
            return Err(ViewError::Revoked.into());
        }
        *pending.request.borrow_mut() = request;
        if reveal {
            self.accept_selection_reveal(commit)?;
        }
        self.arm_word_successor(&pending, Some(commit))
    }

    /// Interpret one original event per turn; original Input publishes all marked state within synchronous callbacks.
    pub(in crate::native_surface::paged_body) fn advance_native_input(
        &self,
        pending: &Rc<PendingWordDelete>,
    ) -> Result<bool> {
        let slot = {
            let mut steps = pending.steps.borrow_mut();
            if !matches!(steps.front(), Some(WordInput::Native(_))) {
                return Ok(false);
            }
            match steps.pop_front() {
                Some(WordInput::Native(slot)) => slot,
                _ => unreachable!("原同次借用已核Native队首"),
            }
        };
        let input = slot.borrow_mut().take().ok_or(ViewError::Busy)?;
        let bytes = input.bytes();
        let serial = self.serial.get();
        let result: Result<bool> = (|| {
            if !pending.current_native_at(self, serial) {
                return Err(ViewError::Revoked.into());
            }
            if let Some(producer) = self.producer.borrow_mut().as_mut() {
                producer.target_source_health()?;
                producer.cancel_word_delete();
                producer.cancel_word_group();
                producer.cancel_visual();
            } // The preceding query finished; Native callbacks cannot reuse its old bound/Ready facts.
            pending.bound.borrow_mut().take();
            pending.ready.borrow_mut().take();
            pending.visual_seed.set(None);
            pending.visual_ready.set(None);
            pending.native_insert_at.set(0);
            let view = self.view()?;
            pending.interpreting_native.set(true);
            let result = match &input {
                // Only the queue head enters these original View APIs. They retain
                // production interpretation, command semantics, and synchronous
                // Input publication instead of reconstructing events or IME state.
                NativeInput::Key { event, .. } => view.interpret_queued_key(event),
                NativeInput::Command(selector) => {
                    view.perform_queued_command(*selector).map(|()| true)
                }
                NativeInput::Paste(text) => view.perform_queued_paste(text).map(|()| true),
                NativeInput::Attachments { paths, drop } => {
                    match view.perform_queued_attachments(paths, *drop) {
                        Err(cause @ ViewError::AttachmentImport(_)) => {
                            // External-file rejection ends this action only; canonical/Body remain editable and already-admitted successors continue normally.
                            let mut failure = pending.attachment_failure.borrow_mut();
                            if failure.is_none() {
                                *failure = Some(cause);
                            } // Retain the chain's first business rejection only; actual Source/resource failures still propagate through Err to Body.
                            Ok(true)
                        }
                        other => other.map(|()| true), // Core failure or committed display effects remain failures, never ordinary import rejection.
                    }
                }
                NativeInput::Format(action) => view.perform_queued_format(*action).map(|()| true),
                NativeInput::InsertTable => view.perform_queued_table_insertion().map(|()| true),
                NativeInput::Fold(action) => {
                    match view.perform_queued_source_fold(action.clone()) {
                        Err(cause @ ViewError::FoldAction(_)) => {
                            let mut failure = pending.fold_failure.borrow_mut();
                            if failure.is_none() {
                                *failure = Some(cause);
                            }
                            Ok(true) // Ordinary business rejection consumes this menu only; later admitted text still settles through the same chain.
                        }
                        other => other.map(|()| true),
                    }
                }
            };
            result.map_err(Into::into)
        })();
        pending.interpreting_native.set(false);
        pending.native_insert_at.set(0);
        drop(input); // Return byte allowance after actual event/payload release; a cancelled local Rc must not retain Native objects.
        pending.queued_bytes.set(pending.queued_bytes.get() - bytes);
        if matches!(result, Ok(false)) {
            self.cancel_original_word_delete(pending)?; // Focus actually transferred; return the old event without claiming it was interpreted.
            self.schedule()?;
            return Ok(true);
        }
        let result = result.and_then(|_| self.arm_word_successor(pending, None));
        self.word_input_result(pending, serial, serial, result)?;
        Ok(true)
    }

    /// An uninterpreted event awaits actual caret Scene. Owned viewport operations transfer only this API call's actual geometry.
    pub(in crate::native_surface::paged_body) fn native_preparation_geometry<T>(
        &self,
        before: &NativeMarkdownPagedSceneRequest,
        viewport: &Rc<crate::native_surface::paged_viewport::NativeMarkdownPagedViewport>,
        operation: impl FnOnce() -> std::result::Result<
            T,
            crate::native_surface::paged_viewport::NativeMarkdownPagedViewportError,
        >,
    ) -> std::result::Result<
        T,
        crate::native_surface::paged_viewport::NativeMarkdownPagedViewportError,
    > {
        use crate::native_surface::paged_viewport::NativeMarkdownPagedViewportError as GeometryError;
        let pending = self.pending_word_delete.borrow().clone();
        let Some(pending) = pending.filter(|pending| {
            pending.native_front()
                && !pending.interpreting_native.get()
                && pending.waiting_vertical.borrow().is_none()
        }) else {
            return operation(); // Ordinary paint and original vertical continuation require no additional input graph or Native getters.
        };
        let serial = self.serial.get();
        if !pending.current_native_at(self, serial) || !pending.request().same(before) {
            return Err(GeometryError::Revoked);
        }
        let view = self.view().map_err(|_| GeometryError::Closed)?;
        let scroll = view.enclosingScrollView();
        if !pending.current_native_at(self, serial) {
            return Err(GeometryError::Revoked);
        }
        let scroll = scroll.ok_or(GeometryError::Closed)?;
        let frame = scroll.frame();
        if !pending.current_native_at(self, serial) {
            return Err(GeometryError::Revoked);
        }
        let bounds = scroll.bounds();
        if !pending.current_native_at(self, serial) {
            return Err(GeometryError::Revoked);
        }
        let value = operation()?; // The original API checked actual target Y, Native mount, bounded frame, and same Input; failure cannot acquire renewed permission.
        let geometry = viewport.geometry()?;
        if !pending.current_at(self, serial) || !geometry.is_current() {
            return Err(GeometryError::Revoked);
        }
        let after_frame = scroll.frame();
        if !pending.current_at(self, serial) || !geometry.is_current() {
            return Err(GeometryError::Revoked);
        }
        let after_bounds = scroll.bounds();
        if !pending.current_at(self, serial)
            || !geometry.is_current()
            || after_frame != frame
            || after_bounds != bounds
        {
            return Err(GeometryError::Revoked); // Outer resize/movement is outside this owned content-height/reveal operation's authority.
        }
        let actual = view.capture_scene_request()?;
        if !pending.current_at(self, serial)
            || !geometry.is_current()
            || actual.input != before.input
            || !actual.target.same(&before.target)
            || !Weak::ptr_eq(&actual.document, &before.document)
            || actual.frame_in_parent != geometry.body_frame
            || actual.virtual_viewport.document_origin != geometry.body_frame.origin
            || actual.virtual_viewport.scroll_epoch != geometry.scroll_epoch
            || actual.virtual_viewport.layout_width != before.virtual_viewport.layout_width
            || actual.virtual_viewport.layout_origin_x != before.virtual_viewport.layout_origin_x
        {
            return Err(GeometryError::Revoked);
        }
        *pending.request.borrow_mut() = actual; // Only this original API's success with current original identity may hand back remaining events under the same ticket.
        Ok(value)
    }

    /// A vertical command from the original event follows that event; the next key waits for its actual final receipt.
    pub(in crate::native_surface::paged_body) fn attach_native_vertical(
        &self,
        vertical: &Rc<super::super::navigation::PendingVertical>,
    ) -> Result<()> {
        let pending = self.pending_word_delete.borrow().clone();
        let Some(pending) = pending.filter(|pending| pending.interpreting_native.get()) else {
            return Ok(());
        };
        if !pending.current_native_at(self, self.serial.get()) || !vertical.current_native(self) {
            return Err(ViewError::Revoked.into());
        }
        *pending.waiting_vertical.borrow_mut() = Some(Rc::clone(vertical));
        Ok(())
    }

    /// Only the original vertical continuation's final result releases its event barrier; external selection cannot impersonate its before-state or pending identity.
    pub(in crate::native_surface::paged_body) fn bind_vertical_native_receipt(
        &self,
        vertical: &Rc<super::super::navigation::PendingVertical>,
        before: &NativeMarkdownPagedSceneRequest,
        commit: Option<Rc<NativeMarkdownPagedInputCommit>>,
    ) -> Result<bool> {
        let Some(pending) = self.pending_word_delete.borrow().clone() else {
            return Ok(false);
        };
        let waiting = pending.waiting_vertical.borrow().clone();
        if !waiting
            .as_ref()
            .is_some_and(|actual| Rc::ptr_eq(actual, vertical))
        {
            return Ok(false);
        }
        let serial = self.serial.get();
        if !pending.current_at_before_commit(self, serial, before.input) {
            return Err(ViewError::Revoked.into());
        }
        let mut expected = before.clone();
        if let Some(commit) = &commit {
            commit
                .check_current(&|epoch| self.current(epoch))
                .map_err(ViewError::from)?;
            expected.input = commit.state;
        }
        let actual = self.view()?.capture_scene_request()?;
        if !actual.same(&expected) {
            return Err(ViewError::Revoked.into());
        }
        if let Some(commit) = &commit {
            commit
                .check_current(&|epoch| self.current(epoch))
                .map_err(ViewError::from)?;
        }
        pending.waiting_vertical.borrow_mut().take();
        *pending.request.borrow_mut() = actual;
        *pending.receipt.borrow_mut() = commit;
        Ok(true)
    }

    /// The keyboard chain receiving this same vertical receipt certifies its next permit; ordinary vertical interaction without a waiter stays unchanged.
    pub(in crate::native_surface::paged_body) fn finish_vertical_native_receipt(
        &self,
        commit: Option<&NativeMarkdownPagedInputCommit>,
    ) -> Result<()> {
        let pending = self
            .pending_word_delete
            .borrow()
            .clone()
            .ok_or(ViewError::Revoked)?;
        self.arm_word_successor(&pending, commit)
    }

    /// Inspect actual input-barrier presence only, without acquiring Source or creating a default queue.
    pub(in crate::native_surface::paged_body) fn native_input_pending(&self) -> bool {
        self.pending_word_delete.borrow().is_some() || self.pending_vertical.borrow().is_some()
    }
    pub(in crate::native_surface::paged_body) fn native_front_needs_scene(&self) -> bool {
        self.pending_word_delete
            .borrow()
            .as_ref()
            .is_some_and(|pending| pending.native_front())
    }
    pub(in crate::native_surface::paged_body) fn native_waits_for_vertical(&self) -> bool {
        self.pending_word_delete
            .borrow()
            .as_ref()
            .is_some_and(|pending| pending.waiting_vertical.borrow().is_some())
    }
}

impl PendingWordDelete {
    /// Inspect queue head only; later events cannot take original Composer continuation from unfinished Word work.
    pub(super) fn native_front(&self) -> bool {
        matches!(self.steps.borrow().front(), Some(WordInput::Native(_)))
    }
    /// After Core commit but before Native notification, compare original before-state and the same existing owner/epoch only.
    fn current_at_before_commit(
        &self,
        owner: &NativeMarkdownPagedBody,
        serial: u64,
        before: NativeMarkdownPagedInputState,
    ) -> bool {
        owner.current_body()
            && owner.serial.get() == serial
            && owner
                .pending_word_delete
                .borrow()
                .as_ref()
                .is_some_and(|actual| std::ptr::eq(&**actual, self))
            && self.request().input == before
    }
}
