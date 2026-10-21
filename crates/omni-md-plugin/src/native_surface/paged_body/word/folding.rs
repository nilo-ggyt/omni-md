//! When word input encounters an opaque fold, retire reads, expand that same anchor, and recompute actual graphemes without consuming the queued action.

use super::*;

impl NativeMarkdownPagedBody {
    /// Text transactions and fold continuation share actual read-graph retirement; return all owners on failure while retaining Source and cleanup causes.
    pub(super) fn retire_word_reading(
        &self,
        pending: &Rc<PendingWordDelete>,
        permit: &NativeMarkdownPagedInputPermit,
        serial: u64,
        released_serial: u64,
        requires_query: bool,
    ) -> Result<()> {
        let before = |epoch| self.current(epoch) && pending.current_native_at(self, serial);
        self.input
            .check_permit(permit, &before)
            .map_err(ViewError::from)?;
        self.view()?.release_scene_readers()?;
        self.input
            .check_permit(permit, &before)
            .map_err(ViewError::from)?;
        let resources = self
            .producer
            .borrow()
            .as_ref()
            .map(|producer| {
                producer.target_source_health()?;
                producer.word_retirement_resources()
            })
            .transpose()?;
        if requires_query && resources.is_none() {
            return Err(ViewError::Revoked.into());
        } // Actual range queries require the original Producer; ordinary admitted insertion may not have created a read graph yet.
        self.serial.set(released_serial);
        pending.retired_serial.set(Some(released_serial));
        self.scheduled.set(None);
        self.deferred.set(false);
        let scene = self.scene.borrow_mut().take();
        let producer = self.producer.borrow_mut().take();
        drop(scene); // End slot borrows before returning original Reader/pixel claims, allowing synchronous release callbacks without a borrowed slot.
        let producer_result = producer.map_or(Ok(()), Producer::retire);
        let worker = self.worker.borrow_mut().take();
        let worker_result = worker.map_or(Ok(()), FontWorker::close);
        let retirement =
            super::super::producer::merge_preparation_cleanup(producer_result, worker_result);
        self.font_notices.borrow_mut().clear();
        let released = resources
            .as_ref()
            .and_then(|resources| resources.snapshot());
        if let Some(errno) = released.as_ref().and_then(|state| state.first_source_errno) {
            let source = ViewError::Document(
                omni_markdown_core::MarkdownDocumentBufferError::OriginalBackingUnavailable(errno),
            );
            return super::super::producer::merge_preparation_cleanup(
                Err(source.into()),
                retirement,
            );
        } // Backing failure discovered at final Drop still precedes ordinary cancellation, while a second actual cleanup failure is retained.
        retirement?;
        if resources.is_some() && released.is_none_or(|state| !state.is_released()) {
            return Err(NativeMarkdownPagedBodyError::ResourceReleaseFailed);
        }
        let current =
            |epoch| self.current(epoch) && pending.current_native_at(self, released_serial);
        self.input
            .check_permit(permit, &current)
            .map_err(ViewError::from)?;
        Ok(())
    }

    /// Consume the fold presentation receipt only. Original Delete/Move/Select remains queued, and the next quantum resolves the complete GCB again.
    pub(super) fn commit_fold_expansion(
        &self,
        pending: &Rc<PendingWordDelete>,
        hidden: Range<usize>,
        serial: u64,
        released_serial: u64,
    ) -> Result<()> {
        let permit = pending
            .permit
            .borrow_mut()
            .take()
            .ok_or(ViewError::Revoked)?;
        self.retire_word_reading(pending, &permit, serial, released_serial, true)?;
        let current =
            |epoch| self.current(epoch) && pending.current_native_at(self, released_serial);
        let outcome = self
            .input
            .reveal_folded_range(&permit, hidden, &current)
            .map_err(ViewError::from)?;
        let super::super::super::paged_input::NativeMarkdownPagedInputOutcome::Committed(commit) =
            outcome
        else {
            return Err(ViewError::Revoked.into());
        }; // The original opaque proof must match a still-existing fold; without actual expansion the action has not resumed successfully.
        let commit = Rc::new(commit);
        *pending.receipt.borrow_mut() = Some(Rc::clone(&commit));
        let synchronized = self
            .synchronize_committed_gutter(&commit, &pending.request())
            .map_err(|cause| ViewError::CommittedEffect(Box::new(cause)))?;
        *pending.request.borrow_mut() = synchronized;
        self.view()?.finish_word_delete_commit(&commit)?;
        self.arm_word_successor(pending, Some(&commit))
    }
}
