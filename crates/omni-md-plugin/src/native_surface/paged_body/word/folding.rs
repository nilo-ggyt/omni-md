//! 原词组动作遇 opaque fold 时先结束读取、展开同一锚点，再保留队首次数重算真实原字素。

use super::*;

impl NativeMarkdownPagedBody {
    /// 文字事务和折叠重试共用真实读图退休边界；失败先归还所有 owner，再保留源与清理首因。
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
        } // 真实范围查询必须来自原 Producer；普通已接纳新增文本可以尚未建立读图。
        self.serial.set(released_serial);
        pending.retired_serial.set(Some(released_serial));
        self.scheduled.set(None);
        self.deferred.set(false);
        let scene = self.scene.borrow_mut().take();
        let producer = self.producer.borrow_mut().take();
        drop(scene); // 所有槽借用先结束，再归还原 Reader/像素；释放同步回调不能重入一个借用中的槽。
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
        } // 原 backing 在最后 Drop 才暴露的失败仍先于普通取消，另一真实清理失败同时保留。
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

    /// 只消费 fold 的显示收据；原 Delete/Move/Select 队首仍在，下一原量子重新取得完整 GCB。
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
        }; // 原同票 opaque proof 必须对应仍存在的 fold；没有实际展开不能声称动作已经重试。
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
