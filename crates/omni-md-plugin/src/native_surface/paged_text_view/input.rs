//! Canonical input transactions and bounded protocol reads end every Core borrow
//! before calling NSInputContext or Native geometry.

use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, ClassType, DefinedClass};
use objc2_foundation::{NSAttributedString, NSPoint, NSRange, NSString};

use super::{
    NativeMarkdownPagedCharacterRect, NativeMarkdownPagedInputCommit,
    NativeMarkdownPagedInputOutcome, NativeMarkdownPagedTextView, NativeMarkdownPagedTextViewError,
    Origin, Result, SceneUse, bounded_viewport, finite_rect,
};

const SUBSTRING_UNITS: usize = 4096;

impl NativeMarkdownPagedTextView {
    /// Publish the attachment link through the original Input, confirm the same provisional file, then
    /// send reentrant Native notifications.
    /// Core rejection leaves file rollback with the caller; failure to display a committed link must
    /// not delete its attachment.
    pub(in crate::native_surface) fn insert_attachment_link(
        &self,
        before: super::NativeMarkdownPagedInputState,
        text: &str,
        interpreted: bool,
        confirm_attachment: impl FnOnce(),
    ) -> Result<()> {
        let _use = crate::payload_use::PayloadUseGuard::enter(&self.ivars().depth);
        let mut origin = self.origin()?;
        origin.interpreted = interpreted; // Only the queue-head attachment caller supplies true; a toolbar cannot claim the original Word receipt from shared depth.
        if origin.state != before {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let current = |epoch| epoch == origin.epoch && origin.current(self);
        let permit = origin.input.capture(origin.epoch, &current)?;
        let commit =
            origin
                .input
                .insert_text_isolated(&permit, text, super::not_found(), &current)?;
        confirm_attachment(); // Confirm the file already referenced by canonical text before display callbacks establish any reentrant state.
        self.finish_edit_commit(&origin, commit)
    }

    /// Save and mode transfer finish composition through the actual NSTextInputClient without copying
    /// or discarding uncommitted marked text.
    pub(in crate::native_surface) fn commit_composition_before_release(&self) -> Result<()> {
        if self.is_busy() {
            return Err(NativeMarkdownPagedTextViewError::Busy);
        }
        // discard may synchronously call the client; retain this view payload through the Native stack
        // and preserve Busy/deferred close ownership.
        let _use = crate::payload_use::PayloadUseGuard::enter(&self.ivars().depth);
        let input = self
            .ivars()
            .input
            .borrow()
            .clone()
            .ok_or(NativeMarkdownPagedTextViewError::Closed)?;
        if input.state()?.marked.is_none() {
            return Ok(()); // No composition means no commit; layout failure must not block canonical save or ordinary close.
        }
        let origin = self.origin()?;
        let context = self
            .ivars()
            .context
            .borrow()
            .clone()
            .ok_or(NativeMarkdownPagedTextViewError::ContextUnavailable)?;
        self.finish_composition_in_context(&input, &origin.delegate, &context, origin.epoch)
    }

    /// Save and actual resignation share the original client's composition cleanup; this is neither new
    /// input nor a recaptured target.
    /// Accept only text already displayed by this Input while allowing the original client's real
    /// callbacks during system-context completion.
    pub(super) fn finish_composition_in_context(
        &self,
        input: &std::rc::Rc<super::NativeMarkdownPagedInput>,
        delegate: &std::rc::Rc<dyn super::NativeMarkdownPagedTextViewDelegate>,
        context: &std::rc::Rc<super::InputContextOwner>,
        epoch: u64,
    ) -> Result<()> {
        if !self.same_input_context(input, delegate, context)
            || self.ivars().epoch != epoch
            || !delegate.current(epoch)
            || !self.same_input_context(input, delegate, context)
        {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        // Save accepts the composition already displayed by the client without choosing a candidate.
        // The original Input first completes
        // Core composition; discard ends only the system conversion session and cannot stand in for
        // insertText or a commit receipt.
        self.cancel_link_intent();
        let mut origin = self.origin()?;
        if !std::rc::Rc::ptr_eq(&origin.input, input)
            || !std::rc::Rc::ptr_eq(&origin.delegate, delegate)
        {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        origin.interpreted =
            self.ivars().interpreting_key.get() && self.ivars().input_callback_depth.get() == 1;
        let current = |epoch| epoch == origin.epoch && origin.current(self);
        let permit = input.capture(epoch, &current)?;
        let unmarked = input.unmark(&permit, &current)?;
        if !self.same_input_context(input, delegate, context)
            || self.ivars().epoch != epoch
            || !delegate.current(epoch)
            || !self.same_input_context(input, delegate, context)
        {
            return Err(NativeMarkdownPagedTextViewError::CommittedEffect(Box::new(
                NativeMarkdownPagedTextViewError::Revoked,
            )));
        }
        // End system conversion before notifying display of the original unmark receipt. Otherwise
        // invalidateCharacterCoordinates
        // may call an IME retaining its old buffer and append old text after local marked state has
        // already cleared.
        // All RefCell/document borrows ended; this closes the original session without using discard to
        // manufacture a Core commit.
        let discarded = self.input_context_step(|| context.0.discardMarkedText());
        let current = self.same_input_context(input, delegate, context)
            && self.ivars().epoch == epoch
            && delegate.current(epoch)
            && self.same_input_context(input, delegate, context);
        if !current {
            return discarded.and(Err(NativeMarkdownPagedTextViewError::CommittedEffect(
                Box::new(NativeMarkdownPagedTextViewError::Revoked),
            )));
        }
        let displayed = self.finish_edit_outcome(&origin, unmarked);
        discarded?;
        displayed?;
        if input.state()?.marked.is_some() {
            return Err(super::NativeMarkdownPagedInputError::CompositionActive.into());
        }
        Ok(())
    }

    /// Synchronous context callbacks borrow only the original client; valid commits may change Input
    /// without adopting a successor mount.
    /// InputTarget is immutable and one Input Rc fixes its original Doc/draft; the Cell content version
    /// changes with real edits,
    /// so an old TargetDomain content_serial cannot validate this callback, and an old Scene cannot
    /// renew authority over new text.
    pub(super) fn same_input_context(
        &self,
        input: &std::rc::Rc<super::NativeMarkdownPagedInput>,
        delegate: &std::rc::Rc<dyn super::NativeMarkdownPagedTextViewDelegate>,
        context: &std::rc::Rc<super::InputContextOwner>,
    ) -> bool {
        !self.ivars().closed.get()
            && self
                .ivars()
                .input
                .borrow()
                .as_ref()
                .is_some_and(|actual| std::rc::Rc::ptr_eq(actual, input))
            && self
                .ivars()
                .delegate
                .borrow()
                .as_ref()
                .is_some_and(|actual| {
                    std::rc::Weak::ptr_eq(actual, &std::rc::Rc::downgrade(delegate))
                })
            && self
                .ivars()
                .context
                .borrow()
                .as_ref()
                .is_some_and(|actual| std::rc::Rc::ptr_eq(actual, context))
    }

    /// NSTextInputClient writebacks return void; recover the first callback failure from this actual
    /// context call.
    /// Preserve and restore earlier view failures without manufacturing keyDown/menu interpretation
    /// authority or allowing old Pending
    /// to masquerade as this conversion failure. No failure-slot or document RefCell borrow crosses the
    /// Native call or callback.
    pub(super) fn input_context_step(&self, operation: impl FnOnce()) -> Result<()> {
        let previous = self.ivars().last_failure.borrow_mut().take();
        operation();
        let callback = self.ivars().last_failure.borrow_mut().take();
        if let Some(previous) = previous {
            self.failure(previous);
        }
        callback.map_or(Ok(()), Err)
    }

    /// Deferred navigation may notify only its original committed receipt; the new origin authorizes
    /// display effects, not a new navigation before-state or position.
    pub(in crate::native_surface) fn finish_navigation_commit(
        &self,
        commit: &NativeMarkdownPagedInputCommit,
    ) -> Result<()> {
        let origin = self
            .origin()
            .map_err(|cause| NativeMarkdownPagedTextViewError::CommittedEffect(Box::new(cause)))?;
        commit
            .check_current(&|epoch| origin.session_current(self, epoch))
            .map_err(|cause| {
                NativeMarkdownPagedTextViewError::CommittedEffect(Box::new(cause.into()))
            })?;
        self.finish_commit_ref(&origin, commit)?;
        origin
            .delegate
            .reveal_committed_selection(commit)
            .map_err(|cause| NativeMarkdownPagedTextViewError::CommittedEffect(Box::new(cause)))
    }

    /// Apply the actual input-client callback under its original permit and replacement range.
    pub(super) fn apply_native_input(
        &self,
        text: &AnyObject,
        replacement: NSRange,
        selected: Option<NSRange>,
    ) -> Result<()> {
        self.cancel_cell_pointer_intent(); // Direct Native insertion or candidate confirmation also cancels the old endpoint, independently of keyDown delivery.
        self.cancel_link_intent(); // Real IME or insertion arrives after the original click, so an asynchronous link must not overtake this input.
        let mut origin = self.origin()?;
        origin.interpreted =
            self.ivars().interpreting_key.get() && self.ivars().input_callback_depth.get() == 1;
        let current = |epoch| epoch == origin.epoch && origin.current(self);
        let permit = origin.input.capture(origin.epoch, &current)?;
        if selected.is_none()
            && replacement.location == objc2_foundation::NSNotFound as usize
            && origin.state.marked.is_none()
            && origin.delegate.defer_insert(text, &permit)?
        {
            return Ok(()); // This accepts ordering only; never insert at the pre-deletion caret or report a commit before it happens.
        }
        // Explicit replacement uses its original range; only synchronous callbacks in the actual
        // key-interpretation stack may hand off later events.
        // Marked input is never queued or reported before publication; an external direct call still
        // cancels original deferred work.
        let composition_anchor = selected.and_then(|_| self.capture_composition_anchor(&origin));
        let commit = match selected {
            Some(selected) => {
                origin
                    .input
                    .set_marked_native(&permit, text, selected, replacement, &current)?
            }
            None => origin
                .input
                .insert_native(&permit, text, replacement, &current)?,
        };
        self.bind_composition_anchor(&origin, &commit, composition_anchor);
        self.finish_edit_commit(&origin, commit)
    }

    /// End marked state through canonical Input before notifying Native display owners.
    pub(super) fn apply_unmark(&self) -> Result<()> {
        self.cancel_link_intent(); // Composition completion or client transfer also ends the original pointer's external-activation authority.
        let mut origin = self.origin()?;
        origin.interpreted =
            self.ivars().interpreting_key.get() && self.ivars().input_callback_depth.get() == 1;
        let current = |epoch| epoch == origin.epoch && origin.current(self);
        let permit = origin.input.capture(origin.epoch, &current)?;
        let outcome = origin.input.unmark(&permit, &current)?;
        self.finish_edit_outcome(&origin, outcome)
    }

    /// Notify only real committed outcomes; unchanged input retains its existing Scene.
    pub(super) fn finish_outcome(
        &self,
        origin: &Origin,
        outcome: NativeMarkdownPagedInputOutcome,
    ) -> Result<()> {
        match outcome {
            NativeMarkdownPagedInputOutcome::Committed(commit) => {
                self.finish_owned_commit(origin, commit, false)
            }
            NativeMarkdownPagedInputOutcome::Unchanged(state) => {
                if state != origin.state || !origin.current(self) {
                    return Err(NativeMarkdownPagedTextViewError::Revoked);
                }
                Ok(()) // No history, no marked text, or identical selection produces neither display notification nor an invented edit.
            }
        }
    }

    /// Typing, composition, deletion, and history request the actual active endpoint visible; ordinary
    /// programmatic selection publishes only its receipt.
    pub(super) fn finish_edit_outcome(
        &self,
        origin: &Origin,
        outcome: NativeMarkdownPagedInputOutcome,
    ) -> Result<()> {
        match outcome {
            NativeMarkdownPagedInputOutcome::Committed(commit) => {
                self.finish_edit_commit(origin, commit)
            }
            unchanged => self.finish_outcome(origin, unchanged), // An unchanged result creates no positioning intent or display frame.
        }
    }

    /// Finish Native notification for the same real receipt before the original Body/Cell Producer
    /// reveals its post-commit caret.
    pub(super) fn finish_edit_commit(
        &self,
        origin: &Origin,
        commit: NativeMarkdownPagedInputCommit,
    ) -> Result<()> {
        self.finish_owned_commit(origin, commit, true)
    }

    /// The original synchronous callback hands over its actual Rc receipt before notification; external
    /// mouse/Find origins hold no interpretation authority.
    pub(super) fn finish_owned_commit(
        &self,
        origin: &Origin,
        commit: impl Into<std::rc::Rc<NativeMarkdownPagedInputCommit>>,
        reveal: bool,
    ) -> Result<()> {
        // Transfer the sole owned receipt; table insertion passes the same Rc to first-Cell activation
        // without recapturing selection to renew authority.
        let commit = commit.into();
        // An empty IME commit consumed and validated by Core may preserve its complete before-state
        // with no text, marked, fold, or selection consequence.
        // Recognize identical complete state only here, where the pre-commit Origin is held;
        // post-commit origins in
        // Find/navigation cannot turn a real commit into a no-op. Interpreted input must still return
        // the same receipt to its original queue.
        let changed = commit.state != origin.state;
        let reveal = reveal && changed;
        let interpreted = origin.interpreted
            && origin
                .delegate
                .begin_interpreted_commit(origin.state, std::rc::Rc::clone(&commit))
                .map_err(|cause| {
                    NativeMarkdownPagedTextViewError::CommittedEffect(Box::new(cause))
                })?;
        if changed {
            self.finish_commit_ref(origin, &commit)?;
        } else {
            // Without display consequences, preserve the Scene, CPU candidate, and user scroll; in
            // particular, an off-screen caret
            // must not create a new scroll intent. Still validate the original owner, epoch, and
            // complete post-state; actual revocation is an error.
            commit
                .check_current(&|epoch| origin.session_current(self, epoch))
                .map_err(|cause| {
                    NativeMarkdownPagedTextViewError::CommittedEffect(Box::new(cause.into()))
                })?;
        }
        if interpreted {
            return origin
                .delegate
                .finish_interpreted_commit(&commit, reveal)
                .map_err(|cause| {
                    NativeMarkdownPagedTextViewError::CommittedEffect(Box::new(cause))
                });
        }
        if reveal {
            origin
                .delegate
                .reveal_committed_selection(&commit)
                .map_err(|cause| {
                    NativeMarkdownPagedTextViewError::CommittedEffect(Box::new(cause))
                })?;
        } // Ordinary direct input displays through its original receipt; absent a barrier, no additional default event owner is created.
        Ok(())
    }

    /// Interpret the queue-head event once; false means focus transferred first, so the Body cancels
    /// this chain rather than reporting callback failure.
    pub(in crate::native_surface) fn interpret_queued_key(
        &self,
        event: &objc2_app_kit::NSEvent,
    ) -> Result<bool> {
        let origin = self.origin()?;
        let window = origin
            .step(self, || self.window())?
            .ok_or(NativeMarkdownPagedTextViewError::Revoked)?;
        let responder = origin.step(self, || window.firstResponder())?;
        if !responder.is_some_and(|responder| {
            std::ptr::eq::<objc2_app_kit::NSResponder>(&*responder, self.as_super().as_super())
        }) {
            // Focus may have moved to Find or another window while queued; the old body cannot write
            // subsequent keys into its former document.
            // Cancel only this uninterpreted event; never replay it into a new client or rewrite the
            // system replacementRange.
            return Ok(false);
        }
        let (_, context) = self.foreground_context()?;
        self.with_interpreted_input(|| {
            if !origin.session_current(self, origin.epoch) {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            // A deferred key enters the original client context once; actual IME callbacks follow the
            // original interpretation stack and Commit handoff,
            // without legacy direct-character interpretation or renewed authority from the old
            // selection after consumption.
            let consumed = context.0.handleEvent(event);
            if !origin.session_current(self, origin.epoch) {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            if !consumed {
                // NO permits original superclass propagation only, not another interpretation;
                // with_interpreted_input preserves the original callback failure.
                // SAFETY: The live main-thread NSView and retained original NSEvent outlive the
                // synchronous superclass keyDown: call.
                unsafe {
                    let _: () = objc2::msg_send![super(self), keyDown: event];
                }
                if !origin.session_current(self, origin.epoch) {
                    return Err(NativeMarkdownPagedTextViewError::Revoked);
                }
            }
            Ok(())
        })?;
        Ok(true)
    }

    /// Execute a deferred public-menu command through the same typed implementation without recursively
    /// entering public queuing.
    pub(in crate::native_surface) fn perform_queued_command(
        &self,
        selector: objc2::runtime::Sel,
    ) -> Result<()> {
        self.with_interpreted_input(|| {
            let _callback =
                crate::payload_use::PayloadUseGuard::enter(&self.ivars().input_callback_depth);
            self.command_from_input_context(selector)
        })
    }

    /// Queue-head formatting uses the same Core implementation and explicitly returns this Commit to
    /// subsequent input; public UI does not infer ownership from depth.
    pub(in crate::native_surface) fn perform_queued_format(
        &self,
        action: omni_markdown_core::MarkdownFormatAction,
    ) -> Result<()> {
        self.with_interpreted_input(|| {
            let _callback =
                crate::payload_use::PayloadUseGuard::enter(&self.ivars().input_callback_depth);
            let _use = crate::payload_use::PayloadUseGuard::enter(&self.ivars().depth);
            self.cancel_link_intent();
            let mut origin = self.origin()?;
            origin.interpreted = true; // Only this controlled queue head certifies the event; shared depth does not authorize reentrant toolbar input.
            self.apply_format_at(&origin, action)
        })
    }

    /// Generate the table template and first-Cell caret after the previous receipt; do not queue a
    /// precomputed string or old anchor.
    pub(in crate::native_surface) fn perform_queued_table_insertion(&self) -> Result<()> {
        self.with_interpreted_input(|| {
            let _callback =
                crate::payload_use::PayloadUseGuard::enter(&self.ivars().input_callback_depth);
            let _use = crate::payload_use::PayloadUseGuard::enter(&self.ivars().depth);
            self.cancel_link_intent();
            let mut origin = self.origin()?;
            origin.interpreted = true; // Execute the Native slot once; the public insertion button must enter the queue at its actual arrival position.
            self.insert_default_table_at(&origin)
        })
    }

    /// Queued paste borrows the string copied on arrival; this callback neither rereads the clipboard
    /// nor creates an NSString.
    pub(in crate::native_surface) fn perform_queued_paste(&self, text: &str) -> Result<()> {
        self.with_interpreted_input(|| {
            let _callback =
                crate::payload_use::PayloadUseGuard::enter(&self.ivars().input_callback_depth);
            let mut origin = self.origin()?;
            origin.interpreted = self.ivars().input_callback_depth.get() == 1;
            let current = |epoch| epoch == origin.epoch && origin.current(self);
            let permit = origin.input.capture(origin.epoch, &current)?;
            let commit =
                origin
                    .input
                    .insert_text_isolated(&permit, text, super::not_found(), &current)?;
            self.finish_edit_commit(&origin, commit)
        })
    }

    /// Only one original key/menu interpretation stack holds this flag; preserve the first typed
    /// callback failure instead of falsely reporting IME completion.
    pub(super) fn with_interpreted_input(
        &self,
        operation: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        if self.ivars().interpreting_key.replace(true) {
            return Err(NativeMarkdownPagedTextViewError::Busy);
        }
        let result = operation();
        self.ivars().interpreting_key.set(false);
        let callback = self.ivars().interpretation_failure.borrow_mut().take();
        if let Some(cause) = callback {
            return Err(cause);
        }
        result
    }

    /// Deferred Find positioning retains the same receipt; display notification borrows the original
    /// Commit without renewing it after movement.
    pub(super) fn finish_commit_ref(
        &self,
        origin: &Origin,
        commit: &NativeMarkdownPagedInputCommit,
    ) -> Result<()> {
        let result = (|| {
            let current = |epoch| origin.session_current(self, epoch);
            commit.check_current(&current)?;
            self.discardCursorRects(); // Committed complete Input first revokes old link cursor regions; only a completed successor CPU frame rebuilds them.
            self.retire_other_composition_anchor(commit.state);
            let cpu = self.ivars().cpu.borrow().clone();
            if let Some(cpu) = cpu {
                cpu.cancel_candidate();
            } // Committed state cancels the old candidate; release the actual Native context after the borrow ends.
            commit.check_current(&current)?; // Release callbacks may reenter; revocation remains CommittedEffect and never authorizes replaying the Core transaction.
            origin.delegate.committed(commit)?;
            commit.check_current(&current)?;
            // Only the committed receipt authorizes notification; neither old before-state nor
            // recaptured successor generation/selection can renew it.
            let geometry = self.ivars().geometry_epoch.get();
            let visible = self.visibleRect();
            commit.check_current(&current)?;
            if geometry != self.ivars().geometry_epoch.get() {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            let bounds = self.bounds();
            commit.check_current(&current)?;
            if geometry != self.ivars().geometry_epoch.get() {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            if let Some(visible) = bounded_viewport(visible, bounds) {
                self.setNeedsDisplayInRect(visible);
                commit.check_current(&current)?;
                if geometry != self.ivars().geometry_epoch.get() {
                    return Err(NativeMarkdownPagedTextViewError::Revoked);
                }
            } // An unmounted view has no actual positive-area viewport; retain accepted canonical input without manufacturing drawing Ready.
            // Committed text does not mean new glyph coordinates are published; an immediate IME
            // requery could replace valid cached geometry with Pending.
            // Body::committed already retired the old Scene and scheduled the sole layout; only
            // scene_ready for the same actual
            // Input, geometry, and context invalidates coordinates. Intermediate states revoked by
            // later input must never report completion.
            // Save, composition completion, and close retain their own original-context cleanup without
            // depending on display completion.
            Ok(())
        })();
        // Core already published; an error may stop display effects but must not imply that canonical
        // text retains its old value.
        result.map_err(|cause| NativeMarkdownPagedTextViewError::CommittedEffect(Box::new(cause)))
    }

    /// Supply the system a bounded UTF-16 window, adjusting edges only to preserve surrogate pairs.
    pub(super) fn substring(
        &self,
        range: NSRange,
    ) -> Result<Option<(Retained<NSAttributedString>, NSRange)>> {
        let origin = self.origin()?;
        if range.location == objc2_foundation::NSNotFound as usize
            || range.location > origin.state.utf16_len
        {
            return Ok(None);
        }
        let requested_end = range
            .location
            .checked_add(range.length)
            .ok_or(NativeMarkdownPagedTextViewError::InvalidRange)?;
        let mut start = range.location;
        let mut end = requested_end
            .min(origin.state.utf16_len)
            .min(start.saturating_add(SUBSTRING_UNITS));
        let mut units = [0u16; SUBSTRING_UNITS];
        let current = |epoch| epoch == origin.epoch && origin.current(self);
        let permit = origin.input.capture(origin.epoch, &current)?;
        let mut unit = [0u16; 1];
        // Read bounded actual UTF-16 under the original permit; Cell creates no snapshot and Body
        // reuses the existing document window.
        if start < origin.state.utf16_len {
            origin
                .input
                .copy_utf16_code_units(&permit, start, &mut unit, &current)?;
            if (0xdc00..=0xdfff).contains(&unit[0]) {
                start = start
                    .checked_sub(1)
                    .ok_or(NativeMarkdownPagedTextViewError::InvalidRange)?;
            }
        }
        end = end.min(start.saturating_add(SUBSTRING_UNITS));
        if end > start && end < origin.state.utf16_len {
            origin
                .input
                .copy_utf16_code_units(&permit, end, &mut unit, &current)?;
            if (0xdc00..=0xdfff).contains(&unit[0]) {
                end -= 1;
            }
        }
        origin
            .input
            .copy_utf16_code_units(&permit, start, &mut units[..end - start], &current)?;
        // Release document and capsule borrows before Foundation initialization; retain only the fixed
        // array and original target.
        if !origin.current(self) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        // SAFETY: The fixed array covers the bounded length and outlives initialization, which
        // synchronously copies its UTF-16 units.
        let string = origin.step(self, || unsafe {
            NSString::initWithCharacters_length(
                NSString::alloc(),
                NonNull::from(&mut units[0]),
                end - start,
            )
        })?;
        let value = origin.step(self, || {
            NSAttributedString::initWithString(NSAttributedString::alloc(), &string)
        })?;
        Ok(Some((value, NSRange::new(start, end - start))))
    }

    /// Validate a Native UTF-16 range against the same Scene and its original Input permit.
    pub(super) fn range_in_source(&self, scene: &SceneUse, range: NSRange) -> Result<()> {
        let end = range
            .location
            .checked_add(range.length)
            .ok_or(NativeMarkdownPagedTextViewError::InvalidRange)?;
        if end > scene.origin.state.utf16_len {
            return Err(NativeMarkdownPagedTextViewError::InvalidRange);
        }
        let current = |epoch| epoch == scene.origin.epoch && scene.current(self);
        let permit = scene.origin.input.capture(scene.origin.epoch, &current)?;
        scene
            .origin
            .input
            .validate_utf16_range(&permit, range.location..end, &current)?;
        if !scene.current(self) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        Ok(())
    }

    /// Convert only a first line or insertion point certified by an actual Ready Scene; Pending cannot
    /// manufacture a candidate-window anchor.
    pub fn screen_rect(&self, range: NSRange) -> Result<NativeMarkdownPagedCharacterRect> {
        let _use = crate::payload_use::PayloadUseGuard::enter(&self.ivars().depth);
        let scene = self.scene()?;
        self.range_in_source(&scene, range)?;
        let mut value = scene.scene.first_rect(range)?;
        if !scene.current(self) || !finite_rect(value.rect, false) || value.rect.size.height <= 0.0
        {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        self.range_in_source(&scene, value.actual_range)?;
        let window = scene
            .step(self, || self.window())?
            .ok_or(NativeMarkdownPagedTextViewError::Pending)?;
        #[cfg(feature = "native-markdown-runtime-probe")]
        let raw = value.rect;
        let base = scene.step(self, || self.convertRect_toView(value.rect, None))?;
        value.rect = scene.step(self, || window.convertRectToScreen(base))?;
        // A zero-width newline still has actual line height; explicit acceptance builds record only the
        // two Native coordinate conversions
        // to distinguish empty-rectangle conversion from Scene range/layout failure, without reading or
        // printing document text.
        #[cfg(feature = "native-markdown-runtime-probe")]
        if raw.size.width == 0.0 {
            eprintln!(
                "native_zero_width_screen_rect raw={raw:?} base={base:?} screen={:?} actual_range={:?}",
                value.rect, value.actual_range,
            );
        }
        if !finite_rect(value.rect, false) {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        Ok(value)
    }

    /// NSTextInputClient points use screen coordinates; window and bitmap coordinates cannot stand in
    /// for body hit testing.
    pub fn screen_hit(&self, point: NSPoint) -> Result<usize> {
        let _use = crate::payload_use::PayloadUseGuard::enter(&self.ivars().depth);
        if !point.x.is_finite() || !point.y.is_finite() {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        let scene = self.scene()?;
        let window = scene
            .step(self, || self.window())?
            .ok_or(NativeMarkdownPagedTextViewError::Pending)?;
        let base = scene.step(self, || window.convertPointFromScreen(point))?;
        let local = scene.step(self, || self.convertPoint_fromView(base, None))?;
        let index = scene.scene.hit(local)?;
        self.range_in_source(&scene, NSRange::new(index, 0))?;
        Ok(index)
    }
}

#[cfg(feature = "native-input-trace")]
impl NativeMarkdownPagedTextView {
    /// Explicit arming borrows the current View's original Input without changing focus, generating
    /// system keys, or switching input sources.
    pub(in crate::native_surface) fn arm_input_trace(
        &self,
        ticks: u64,
        numer: u32,
        denom: u32,
    ) -> std::result::Result<(), crate::NativeInputTraceAccessError> {
        if self.ivars().closed.get() {
            return Err(crate::NativeInputTraceAccessError::NoEditor);
        }
        if self.is_busy() {
            return Err(crate::NativeInputTraceAccessError::Busy);
        }
        let input = self
            .ivars()
            .input
            .borrow()
            .clone()
            .ok_or(crate::NativeInputTraceAccessError::NoEditor)?;
        input.arm_input_trace(ticks, numer, denom)
    }

    /// The View owns no separate recorder; clone the original Input handle outside the slot borrow and
    /// retain only finite scalar trace payload.
    pub(in crate::native_surface) fn input_trace(
        &self,
    ) -> Option<crate::native_input_trace::SharedNativeInputTrace> {
        let input = self.ivars().input.borrow().clone()?;
        input.input_trace()
    }

    /// Explicit drain creates neither a new Input nor another Native graph; the original TraceScope
    /// reports an active selector as ActiveScope.
    pub(in crate::native_surface) fn drain_input_trace(
        &self,
    ) -> std::result::Result<crate::NativeInputTraceSnapshot, crate::NativeInputTraceAccessError>
    {
        let input = self
            .ivars()
            .input
            .borrow()
            .clone()
            .ok_or(crate::NativeInputTraceAccessError::NoEditor)?;
        input.drain_input_trace()
    }

    /// Observe this client's public marked-state facts synchronously without calling NSTextInputContext
    /// or reading body text.
    fn input_trace_observation(&self) -> (Option<crate::NativeInputTraceDocument>, bool) {
        let input = self.ivars().input.borrow().clone();
        input.map_or((None, false), |input| input.input_trace_observation())
    }

    /// Record event provenance only inside the actual keyDown stack; later queued interpretation cannot
    /// claim that the old event is still on-stack.
    pub(super) fn begin_input_trace_event(
        &self,
        event: &objc2_app_kit::NSEvent,
    ) -> Option<crate::native_input_trace::TraceScope> {
        let trace = self.input_trace()?;
        let ticks = crate::native_surface::input_trace::native_input_trace_now();
        let timestamp_bits = event.timestamp().to_bits(); // No document or RefCell borrow survives this Native getter.
        let (document, marked) = self.input_trace_observation();
        crate::native_input_trace::TraceScope::event(
            &trace,
            ticks,
            timestamp_bits,
            document,
            marked,
        )
    }

    /// Record the actual selector for asynchronous system IME callbacks; absent a current event, its
    /// event identity remains None.
    pub(super) fn begin_input_trace_selector(
        &self,
        selector: crate::NativeInputTraceSelector,
    ) -> Option<crate::native_input_trace::TraceScope> {
        let trace = self.input_trace()?;
        let (document, marked) = self.input_trace_observation();
        crate::native_input_trace::TraceScope::selector(
            &trace,
            selector,
            crate::native_surface::input_trace::native_input_trace_now(),
            document,
            marked,
        )
    }

    /// Close the same scope after its selector and effects return; no output, observer registration, or
    /// successor callback is created.
    pub(super) fn finish_input_trace_scope(
        &self,
        scope: Option<crate::native_input_trace::TraceScope>,
    ) {
        if let Some(scope) = scope {
            if self.input_trace().is_none() {
                return; // Input::close retired the payload; Drop records aborted rather than treating missing marked state as composition confirmation.
            }
            let (document, marked) = self.input_trace_observation();
            scope.finish(
                crate::native_surface::input_trace::native_input_trace_now(),
                document,
                marked,
            );
        }
    }

    /// Map admission failures only to existing finite categories; never record typed errors or
    /// sensitive business strings.
    pub(super) fn reject_input_trace(&self, cause: &NativeMarkdownPagedTextViewError) {
        let Some(input) = self.ivars().input.borrow().clone() else {
            return;
        };
        let rejection = match cause {
            NativeMarkdownPagedTextViewError::Closed
            | NativeMarkdownPagedTextViewError::Input(
                super::NativeMarkdownPagedInputError::Closed,
            ) => crate::NativeInputTraceRejection::Closing,
            NativeMarkdownPagedTextViewError::Input(
                super::NativeMarkdownPagedInputError::DocumentReleased,
            ) => crate::NativeInputTraceRejection::DocumentUnavailable,
            _ => crate::NativeInputTraceRejection::CanonicalError,
        };
        input.reject_input_trace(rejection);
    }
}
