//! A Find request binds the normal Producer's original Source once. Selection commit
//! and an old Ready frame cannot substitute for actual target visibility.

use super::*;
use crate::native_surface::paged_input::NativeMarkdownPagedInputOutcome;
use crate::native_surface::{
    NativeMarkdownController,
    find_bar::{
        MarkdownFindAction, NativeMarkdownFindBackend, NativeMarkdownFindBar,
        NativeMarkdownFindExecution, NativeMarkdownFindExecutionError,
        NativeMarkdownPagedFindBackend, NativeMarkdownPagedFindRevealPort,
        NativeMarkdownPagedFindRevealProgress, NativeMarkdownPagedFindRevealRequest,
        execute_find_backend,
    },
};
use target::SourceTargetRequest;

/// Keep the original reveal request and one eventual Source target. Admission records
/// wrapping geometry without acquiring a reader or a second input owner.
pub(super) struct PendingFind {
    request: Rc<NativeMarkdownPagedFindRevealRequest>,
    layout_width: f64,
    layout_origin_x: f64,
    bound: Cell<bool>,
    target: RefCell<Option<Rc<SourceTargetRequest>>>,
}

impl NativeMarkdownPagedFindRevealPort for NativeMarkdownPagedBody {
    fn cancel_pending(&self) {
        if let Err(cause) = self.cancel_pending_find() {
            self.fail(cause); // Preserve the original Source failure; ordinary cancellation cannot swallow its existing errno.
        }
    }

    /// Admit the complete original document/Input request before the normal Producer
    /// chooses its Source. The result remains Pending until actual visibility is certified.
    fn begin(
        &self,
        request: Rc<NativeMarkdownPagedFindRevealRequest>,
    ) -> std::result::Result<NativeMarkdownPagedFindRevealProgress, String> {
        self.cancel_pending_find()
            .map_err(|cause| cause.to_string())?;
        self.find_source_health()
            .map_err(|cause| cause.to_string())?;
        if !self.current_body() || !request.is_current() {
            return Err("原Find请求已经失效".into());
        }
        let actual = self
            .view()
            .map_err(|cause| cause.to_string())?
            .capture_scene_request()
            .map_err(|cause| cause.to_string())?;
        if request.epoch() != self.configuration.epoch
            || request.input_state() != actual.input
            || !actual.target.is_body(&actual.document)
            || !Weak::ptr_eq(&request.document(), &actual.document)
            || !Weak::ptr_eq(&actual.document, &self.input.document_identity())
        {
            return Err("原Find请求不属于此正文".into());
        }
        self.find_source_health()
            .map_err(|cause| cause.to_string())?;
        let current = request.is_current() && self.current_body();
        self.find_source_health()
            .map_err(|cause| cause.to_string())?;
        if !current {
            return Err("原Find请求在接收时已经失效".into());
        }
        let pending = Rc::new(PendingFind {
            request,
            layout_width: actual.virtual_viewport.layout_width,
            layout_origin_x: actual.virtual_viewport.layout_origin_x,
            bound: Cell::new(false),
            target: RefCell::new(None),
        });
        // apply_find_commit already cleared the Scene. Store only this ticket and
        // actual width, requiring neither old Ready nor a Source/Reader acquisition.
        // Normal advance selects the original Producer before the one permitted binding.
        drop(self.pending_page_scroll.borrow_mut().take()); // Later Find reveal owns the viewport; an old scroll-only sequence cannot resume across it.
        *self.pending_find.borrow_mut() = Some(pending.clone());
        if let Err(cause) = self.schedule() {
            self.retire_find(&pending, Some(cause.to_string()));
            return Err(cause.to_string());
        }
        Ok(NativeMarkdownPagedFindRevealProgress::Pending)
    }
}

impl NativeMarkdownPagedBody {
    /// Finish the actual active cell before Find reads canonical text. Its post-commit
    /// ticket must remain valid; a successor selection cannot renew the old action.
    pub(in crate::native_surface) fn finish_cell_before_find(
        &self,
        controller: &NativeMarkdownController,
        intent: u64,
    ) -> Result<()> {
        if self.active_cell_body().is_none() {
            return Ok(()); // Ordinary Body Find has no cell-commit obligation; unrelated drawing does not add an admission barrier.
        }
        let Some(committed) = self.commit_active_cell()? else {
            return Err(ViewError::Revoked.into()); // The original draft must finish or lose authority; a vanished commit boundary cannot acquire successor input.
        };
        let current = |epoch| {
            self.current(epoch)
                && self.active_cell_body().is_none()
                && controller.find_input_intent() == intent
                && crate::native_surface::native_markdown_controller_has_foreground_authority(
                    controller,
                )
        };
        // Detachment enters Native focus APIs. After reentry validate the original
        // after-state instead of recapturing current selection to renew permission.
        let result = match committed.outcome {
            NativeMarkdownPagedInputOutcome::Committed(after) => {
                after.check_current(&current).map_err(ViewError::Input)
            }
            NativeMarkdownPagedInputOutcome::Unchanged(after) => {
                if current(self.configuration.epoch)
                    && self.input.state().is_ok_and(|state| state == after)
                {
                    Ok(())
                } else {
                    Err(ViewError::Revoked)
                }
            }
        };
        // The draft is already sealed. Later revocation is a committed effect,
        // not permission for the caller to repeat the original edit.
        result.map_err(|cause| ViewError::CommittedEffect(Box::new(cause)).into())
    }

    /// The composition root lends this body's sole Input to the existing backend.
    /// No second Input owner, alternative Find entry, or changed button/command is
    /// introduced; arguments are borrowed for this synchronous action alone.
    pub(in crate::native_surface) fn execute_paged_find_action(
        &self,
        bar: &Rc<NativeMarkdownFindBar>,
        controller: &NativeMarkdownController,
        intent: u64,
        action: MarkdownFindAction,
    ) -> std::result::Result<NativeMarkdownFindExecution, NativeMarkdownFindExecutionError> {
        // The existing bar continues after cell editing; every match/replace entry
        // reads canonical text after this actual draft commit.
        self.finish_cell_before_find(controller, intent)
            .map_err(|cause| NativeMarkdownFindExecutionError::Rejected(cause.to_string()))?;
        let view = self
            .view()
            .map_err(|cause| NativeMarkdownFindExecutionError::Rejected(cause.to_string()))?;
        execute_find_backend(
            bar,
            NativeMarkdownFindBackend::Paged(NativeMarkdownPagedFindBackend {
                view: &view,
                input: &self.input,
                epoch: self.configuration.epoch,
                controller,
                intent,
                reveal: self,
            }),
            action,
        )
    }

    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(in crate::native_surface) fn probe_find_input_weak(
        &self,
    ) -> Weak<NativeMarkdownPagedInput> {
        Rc::downgrade(&self.input) // Observe release of the sole Input without handing probes another strong owner.
    }

    #[cfg(all(
        feature = "native-paged-body-probe",
        feature = "native-markdown-runtime-probe"
    ))]
    pub(in crate::native_surface) fn probe_bound_find_source(
        &self,
    ) -> Option<(
        Weak<NativeMarkdownPagedFindRevealRequest>,
        omni_markdown_core::MarkdownLayoutTextAuthority,
    )> {
        let pending = self.pending_find.borrow().clone()?;
        if !pending.bound.get() {
            return None;
        }
        let target = pending.target.borrow().clone()?;
        Some((Rc::downgrade(&pending.request), target.source.clone()))
    }

    /// Exact Rc identity prevents an old callback from consuming a successor Find slot.
    fn find_slot_is(&self, pending: &Rc<PendingFind>) -> bool {
        self.pending_find
            .borrow()
            .as_ref()
            .is_some_and(|actual| Rc::ptr_eq(actual, pending))
    }

    fn find_scope_current(&self, pending: &Rc<PendingFind>) -> bool {
        // End slot borrows before the original request enters Controller/View gates;
        // Native reentry must not encounter a live RefCell borrow here.
        self.current_body()
            && self.find_slot_is(pending)
            && pending.request.is_current()
            && self.find_slot_is(pending)
    }

    /// Check only existing Producer/Source and canonical endpoint health, without
    /// reading document-wide text or acquiring replacement layout authority.
    pub(super) fn find_source_health(&self) -> Result<()> {
        if let Some(producer) = self.producer.borrow().as_ref() {
            producer.target_source_health()?;
        }
        let document = self
            .document
            .borrow()
            .clone()
            .ok_or(NativeMarkdownPagedBodyError::Closed)?;
        document
            .try_borrow()
            .map_err(|_| ViewError::Busy)?
            .byte_offset_for_utf16(0)
            .map_err(ViewError::Document)?;
        Ok(()) // Original canonical endpoint and Source health only; no text materialization or Native calls.
    }

    #[cfg_attr(feature = "native-close-stage-diagnostic", track_caller)]
    /// Settle this request's terminal state and revoke only its exact target identity.
    fn retire_find(&self, pending: &Rc<PendingFind>, failure: Option<String>) {
        #[cfg(feature = "native-close-stage-diagnostic")]
        if failure.is_none() {
            // Ordinary cancellation has no error payload. Record its actual static
            // caller so terminal observation retains the originating branch without
            // rereading Source, Native state, or permission, or changing retirement.
            let caller = std::panic::Location::caller();
            eprintln!(
                "native_find_cancel_requested file={} line={}",
                caller.file(),
                caller.line(),
            );
        }
        let removed = {
            let mut slot = self.pending_find.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|actual| Rc::ptr_eq(actual, pending))
            {
                slot.take()
            } else {
                None // An old callback may revoke its own request, never the installed successor Find slot.
            }
        };
        drop(removed);
        if let Some(cause) = failure {
            pending.request.fail(cause);
        } else {
            pending.request.cancel();
        }
        let target = pending.target.borrow_mut().take();
        if let Some(target) = target {
            let removed = {
                let mut slot = self.source_target.borrow_mut();
                if slot
                    .as_ref()
                    .is_some_and(|actual| Rc::ptr_eq(actual, &target))
                {
                    slot.take()
                } else {
                    None
                }
            };
            drop(removed);
            target.revoke(); // Revoke this exact Find goal; equal numeric ranges cannot identify a successor target.
        }
    }

    /// Original Source failure takes precedence over ordinary cancellation; an
    /// already-cancelled request cannot be revived by a later error.
    pub(super) fn cancel_pending_find(&self) -> Result<()> {
        let pending = self.pending_find.borrow().clone();
        if let Some(pending) = pending {
            let health = self.find_source_health();
            if let Err(cause) = health {
                self.retire_find(&pending, Some(cause.to_string()));
                return Err(cause);
            }
            self.retire_find(&pending, None);
        }
        Ok(()) // Earlier ordinary cancellation is terminal; a later failure cannot revive the old request.
    }

    pub(super) fn fail_pending_find(&self, cause: &NativeMarkdownPagedBodyError) {
        let pending = self.pending_find.borrow().clone();
        if let Some(pending) = pending {
            self.retire_find(&pending, Some(cause.to_string()));
        }
    }

    pub(super) fn find_pending(&self) -> bool {
        self.pending_find.borrow().is_some()
    }

    /// Revoke changed Input, wrapping geometry, owner identity, or an already-bound
    /// goal while preserving Source errors on both sides of foreground validation.
    pub(super) fn refresh_pending_find(
        &self,
        actual: &NativeMarkdownPagedSceneRequest,
    ) -> Result<()> {
        let pending = self.pending_find.borrow().clone();
        let Some(pending) = pending else {
            return Ok(());
        };
        self.find_source_health()?;
        let current = self.find_scope_current(&pending)
            && actual.input == pending.request.input_state()
            && actual.virtual_viewport.layout_width == pending.layout_width
            && actual.virtual_viewport.layout_origin_x == pending.layout_origin_x
            && Weak::ptr_eq(&actual.document, &pending.request.document());
        self.find_source_health()?;
        let goal_revoked = pending.bound.get()
            && pending
                .target
                .borrow()
                .as_ref()
                .is_none_or(|target| !target.current(self));
        if !current || goal_revoked {
            self.retire_find(&pending, None);
        }
        Ok(())
    }

    /// Bind the selected Producer's original Source exactly once, outside any
    /// Native request recapture that could accidentally authorize successor input.
    pub(super) fn bind_pending_find(
        &self,
        producer: &Producer,
        actual: &NativeMarkdownPagedSceneRequest,
    ) -> Result<()> {
        let pending = self.pending_find.borrow().clone();
        let Some(pending) = pending else {
            return Ok(());
        };
        producer.target_source_health()?;
        if pending.bound.get() {
            return Ok(()); // A bound goal never acquires a successor Source; losing its original ticket permits revocation only.
        }
        let original_serial = self.serial.get();
        let source = producer.target_source_authority(actual, original_serial)?;
        let scope = || {
            self.find_scope_current(&pending)
                && self.serial.get() == original_serial
                && actual.virtual_viewport.layout_width == pending.layout_width
                && actual.virtual_viewport.layout_origin_x == pending.layout_origin_x
                && actual.input == pending.request.input_state()
                && self.view().is_ok_and(|view| {
                    view.capture_scene_request()
                        .is_ok_and(|request| request.same(actual))
                })
                && self.find_scope_current(&pending)
        };
        let result = self.admit_source_target(actual, source, pending.request.range(), &scope);
        producer.target_source_health()?; // This Producer is on-stack; query cancellation cannot swallow Source failure observed after Native reentry.
        let current = scope();
        producer.target_source_health()?;
        let target = match result {
            Ok(target) if current => target,
            Ok(target) => {
                target.revoke();
                self.retire_find(&pending, None);
                return Ok(());
            }
            Err(cause) if cause.is_foreground_revocation() => {
                self.retire_find(&pending, None);
                return Ok(()); // Original PreparationTicket handles edits/resize; the old Find cannot bind another Source.
            }
            Err(cause) => return Err(cause),
        };
        producer.target_source_health()?;
        if !self.find_slot_is(&pending) || pending.bound.replace(true) {
            target.revoke();
            return Ok(());
        }
        *pending.target.borrow_mut() = Some(target.clone());
        self.install_source_target(target)?;
        Ok(())
    }

    /// Pure SourceTarget.current never enters Native. Its separate stable-target
    /// Native check validates the original Find intent here.
    pub(super) fn find_source_target_current(&self, target: &SourceTargetRequest) -> bool {
        let pending = self.pending_find.borrow().clone();
        let Some(pending) = pending else {
            return true;
        };
        let owns = pending
            .target
            .borrow()
            .as_ref()
            .is_some_and(|actual| std::ptr::eq::<SourceTargetRequest>(&**actual, target));
        !owns || self.find_scope_current(&pending)
    }

    /// Cancel only a goal actually owned by a stale original Find scope; unrelated
    /// targets retain their own admission and cancellation authority.
    pub(super) fn cancel_stale_find_target(&self, target: &SourceTargetRequest) -> Result<bool> {
        let pending = self.pending_find.borrow().clone();
        let Some(pending) = pending else {
            return Ok(false);
        };
        let owns = pending
            .target
            .borrow()
            .as_ref()
            .is_some_and(|actual| std::ptr::eq::<SourceTargetRequest>(&**actual, target));
        if !owns || self.find_scope_current(&pending) {
            return Ok(false);
        }
        self.find_source_health()?;
        self.retire_find(&pending, None);
        Ok(true) // Only the original Find intent ended; callers must still verify PreparationTicket and original Source independently.
    }

    /// Consume the original target's one-shot visibility proof, then settle the
    /// request without treating selection commit or an empty proof slot as completion.
    pub(super) fn finish_pending_find(&self) -> Result<()> {
        let pending = self.pending_find.borrow().clone();
        let Some(pending) = pending else {
            return Ok(());
        };
        let target = pending.target.borrow().clone();
        if target.as_ref().is_some_and(|target| target.pending()) {
            // The sole advance caller already ran refresh_pending_find, Producer
            // advance, and finish_source_target in this turn. Source failures reach
            // fail_pending_find through Result; in-flight work has no new proof.
            // Original cancellation/Input/width gates still revoke it without renewal.
            return Ok(());
        }
        self.find_source_health()?;
        let current = self.find_scope_current(&pending);
        self.find_source_health()?;
        if !current {
            self.retire_find(&pending, None);
            return Ok(());
        }
        let Some(target) = target else {
            return Ok(());
        };
        let proof = self.take_source_target_request_visible(&target);
        self.find_source_health()?;
        let proof = match proof {
            Ok(Some(proof)) => proof,
            Ok(None) => return Err(ViewError::InvalidRange.into()), // A completed target's consumed proof never authorizes retry.
            Err(cause) if cause.is_foreground_revocation() => {
                self.retire_find(&pending, None);
                return Ok(());
            }
            Err(cause) => return Err(cause),
        };
        let result = pending.request.finish_visible(&proof);
        self.find_source_health()?;
        if let Err(cause) = result {
            let current = self.find_scope_current(&pending);
            self.find_source_health()?;
            if !current {
                self.retire_find(&pending, None);
            } else {
                self.retire_find(&pending, Some(cause));
            }
            return Ok(());
        }
        // Completion makes request.is_current false. Remove only the original slot,
        // preserving its actual completed receipt for the caller.
        let mut slot = self.pending_find.borrow_mut();
        if slot
            .as_ref()
            .is_some_and(|actual| Rc::ptr_eq(actual, &pending))
        {
            drop(slot.take());
        }
        Ok(())
    }
}
