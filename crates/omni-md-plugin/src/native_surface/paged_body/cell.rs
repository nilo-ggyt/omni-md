//! 原Cell动作先有限准入，再归还显示读取，最后只消费同张canonical提交票。

use super::*;
use crate::native_surface::paged_input::{
    NativeMarkdownPagedInputOutcome, NativeMarkdownPagedInputPermit,
};

impl NativeMarkdownPagedBody {
    /// 归还这一个Cell的全部显示读取，但保留Input/session和已显示像素供原准入继续。
    pub(super) fn release_cell_readers(&self) -> Result<()> {
        if !self.input.is_cell() {
            return Err(NativeMarkdownPagedBodyError::Unsupported);
        }
        let next = self
            .serial
            .get()
            .checked_add(1)
            .ok_or(NativeMarkdownPagedBodyError::CounterExhausted)?;
        self.view()?.release_scene_readers()?; // 正在同步paint时明确Busy，未发生任何草稿封闭。
        self.serial.set(next);
        self.scheduled.set(None);
        self.deferred.set(false);
        drop(self.pending_vertical.borrow_mut().take());
        drop(self.pending_page_scroll.borrow_mut().take());
        drop(self.scene.borrow_mut().take());
        let producer = self.producer.borrow_mut().take();
        let producer_result = producer.map_or(Ok(()), Producer::retire);
        let worker = self.worker.borrow_mut().take();
        let worker_result = worker.map_or(Ok(()), FontWorker::close);
        self.font_notices.borrow_mut().clear();
        // 两类旧结果都实际消费后才允许下一capture/提交，前一步失败也必须join原worker。
        producer::merge_preparation_cleanup(producer_result, worker_result)
    }

    /// 原Cell与既有canonical Input各执原票，只提交一次；这不是正式表格UI挂载或Tab导航。
    pub fn commit_cell_to_body(
        &self,
        body: &Rc<NativeMarkdownPagedInput>,
        body_permit: &NativeMarkdownPagedInputPermit,
        body_current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputOutcome> {
        if self.preparing.get() || !self.input.is_cell() {
            return Err(NativeMarkdownPagedBodyError::Busy);
        }
        let view = self.view()?;
        let request = view.capture_scene_request()?;
        let current = |epoch| {
            self.current(epoch)
                && view
                    .capture_scene_request()
                    .is_ok_and(|actual| actual.same(&request))
        };
        let permit = self
            .input
            .capture(self.configuration.epoch, &current)
            .map_err(ViewError::Input)?;
        // 原票、marked、文档预算或候选存储准备失败在此返回；
        // 原Scene和原帧均不动，不先为提交清空显示，也不因草稿长而截断内容。
        let prepared = self
            .input
            .prepare_cell_commit(&permit, &current)
            .map_err(ViewError::Input)?;
        self.release_cell_readers()?;
        let result =
            self.input
                .commit_cell_to_body(prepared, body, body_permit, &current, body_current);
        match result {
            Ok(outcome) => {
                drop(self.selection_reveal.borrow_mut().take());
                self.close_finished_cell();
                Ok(outcome) // Core已seal原Cell；只有真实成功后才撤无reader的原caret意图。
            }
            Err(cause) => {
                let error = NativeMarkdownPagedBodyError::CellCommit(cause);
                if let Err(rebuild) = self.schedule() {
                    self.fail(rebuild); // 恢复显示失败保留为原owner首错，不覆盖这次真实提交失败。
                } // 原草稿未改，沿同一Input重建真实Source；不重放已消费的prepared。
                Err(error)
            }
        }
    }

    /// Esc只消费原Cell输入票；原绘图/字体归还后取消同token，Native图在原回调退出后关闭。
    pub(super) fn cancel_cell(
        &self,
        permit: NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<(), ViewError> {
        if self.preparing.get() {
            return Err(ViewError::Busy);
        }
        self.input
            .check_permit(&permit, &|epoch| self.current(epoch))?;
        self.release_cell_readers().map_err(|cause| {
            self.fail(cause);
            ViewError::Revoked
        })?;
        // 真实context/字体析构可重入输入；同一原票必须仍有效，不能取消后继草稿。
        if let Err(cause) = self
            .input
            .check_permit(&permit, &|epoch| self.current(epoch))
        {
            if let Err(rebuild) = self.schedule() {
                self.fail(rebuild); // 析构重入已改变输入时只恢复显示，不重放取消或给旧票续权。
            }
            return Err(cause.into());
        }
        let session = self
            .cell_session
            .borrow()
            .clone()
            .ok_or(ViewError::Closed)?;
        session.close()?;
        drop(self.selection_reveal.borrow_mut().take()); // 同token真实取消以后才撤原意图；拒绝仍保后继显示。
        self.input.close()?;
        self.close_finished_cell();
        Ok(())
    }

    /// 原token已提交或取消后立即撤回调权限；Native拆图等当前输入栈退出，不能再读已归还草稿。
    fn close_finished_cell(&self) {
        self.active.set(false); // 此后旧clip/输入/worker回调均不可重建，实际Native关闭等原栈退出。
        let weak = MainThreadBound::new(self.weak.clone(), self.mtm);
        super::super::enqueue_native_markdown_main_task(move || {
            let mtm = MainThreadMarker::new().expect("单格关闭只在主线程完成");
            if let Some(owner) = weak.get(mtm).upgrade()
                && let Err(cause) = owner.begin_close()
            {
                owner.fail(cause);
            }
        });
    }
}
