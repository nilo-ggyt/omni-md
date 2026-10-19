//! 先有限准入，再由原控制器撤Scene并消费同票；不重新捕获后继草稿或执行第二次sanitize。

use super::*;
use omni_markdown_core::{MarkdownCellDraftCommitError, MarkdownCellDraftPreparedCommit};

#[cfg(test)]
#[path = "cell_commit/body_tests.rs"]
mod body_tests;

/// 唯一结果仍归Core准入claim：小结果保有限堆字符串，长结果保封存匿名输出；
/// 此封闭包只增加原Input/session弱身份和原epoch标量，不复制结果或保活原草稿。
pub struct NativeMarkdownPagedCellPreparedCommit {
    owner: Weak<NativeMarkdownPagedInput>,
    session: Weak<NativeMarkdownPagedCellSession>,
    epoch: u64,
    before: NativeMarkdownPagedInputState,
    prepared: MarkdownCellDraftPreparedCommit,
}

#[derive(Debug)]
pub enum NativeMarkdownPagedCellCommitError<E> {
    Input(NativeMarkdownPagedInputError),
    Commit(MarkdownCellDraftCommitError<E>),
}

impl<E: std::fmt::Display> std::fmt::Display for NativeMarkdownPagedCellCommitError<E> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Input(error) => error.fmt(formatter),
            Self::Commit(error) => error.fmt(formatter),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for NativeMarkdownPagedCellCommitError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Input(error) => Some(error),
            Self::Commit(error) => Some(error),
        }
    }
}

impl<E> From<NativeMarkdownPagedInputError> for NativeMarkdownPagedCellCommitError<E> {
    fn from(error: NativeMarkdownPagedInputError) -> Self {
        Self::Input(error)
    }
}

/// canonical观察者仍消费原一次事务；局部Input不能把提交结果伪装为同一Cell的可继续状态。
pub type NativeMarkdownPagedCellCommitResult<O> = std::result::Result<
    Option<(
        MarkdownHistoryTextEdit,
        <O as MarkdownTextChangeObserver>::Output,
    )>,
    NativeMarkdownPagedCellCommitError<<O as MarkdownTextChangeObserver>::Error>,
>;

impl NativeMarkdownPagedInput {
    /// 消费同一次Cell准入，将唯一canonical事务交给已经存在的Body输入owner。
    /// 两个Scene须先由原控制器撤销并归还；这里不重新prepare、不重建Body，也不补捕新票。
    ///
    /// # Errors
    /// 外来Doc/Cell/Body票、marked、借用或原观察者失败都在Core发布前拒绝并保留草稿。
    /// 成功后的前台变化由返回Body receipt单独报告，绝不把已提交正文解释为未提交。
    pub fn commit_cell_to_body(
        self: &Rc<Self>,
        prepared: NativeMarkdownPagedCellPreparedCommit,
        body: &Rc<NativeMarkdownPagedInput>,
        body_permit: &NativeMarkdownPagedInputPermit,
        cell_current: &(impl Fn(u64) -> bool + ?Sized),
        body_current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> std::result::Result<
        NativeMarkdownPagedInputOutcome,
        NativeMarkdownPagedCellCommitError<NativeMarkdownPagedInputObserverError>,
    > {
        // 不读取外来prepared的Doc；先核原Cell owner，再沿自己的session保Source首因。
        if !Weak::ptr_eq(&prepared.owner, &Rc::downgrade(self)) {
            return Err(NativeMarkdownPagedInputError::Revoked.into());
        }
        let session = prepared
            .session
            .upgrade()
            .ok_or(NativeMarkdownPagedInputError::Closed)?;
        session.facts()?;
        let own_session = self
            .target
            .cell()?
            .ok_or(NativeMarkdownPagedInputError::BodyTargetRequired)?;
        if !Rc::ptr_eq(&own_session, &session) || body.is_cell() || body_permit.cell.is_some() {
            return Err(NativeMarkdownPagedInputError::BodyTargetRequired.into());
        }
        let original = session.document()?;
        if !Weak::ptr_eq(&body.document_identity(), &Rc::downgrade(&original)) {
            return Err(NativeMarkdownPagedInputError::Revoked.into());
        } // 相同正文、相同generation或同一局部range都不能代替原canonical Doc身份。

        self.check_original(prepared.epoch, prepared.before, cell_current)?;
        if self.busy.replace(true) {
            return Err(NativeMarkdownPagedInputError::Busy.into());
        }
        let _cell_mutation = Mutation(&self.busy);
        let _body_mutation = body.begin(body_permit, body_current)?;
        if body_permit.before.marked.is_some() {
            return Err(NativeMarkdownPagedInputError::CompositionActive.into());
        } // Cell提交不能抢走Body自己的IME候选；Core准入已独立拒绝Cell marked。

        // 交叉current都可重入，但两个busy槽已封闭。结束全部回调后才持Doc/folds借用，
        // 下方只有纯Core prepare/publish，观察者不得调用Native或较新的current。
        session.facts()?;
        self.check_original(prepared.epoch, prepared.before, cell_current)?;
        body.check_permit(body_permit, body_current)?;
        self.check_original(prepared.epoch, prepared.before, cell_current)?;
        let observer = body.observer(body_permit.before)?;
        let mut document = original
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        let mut folds = body
            .folds
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        if !body.matches_document(&document, body_permit.before)
            || !self.metadata_matches(prepared.before)
        {
            return Err(NativeMarkdownPagedInputError::StateChanged.into());
        }
        let result = document
            .commit_prepared_cell_draft_observed(prepared.prepared, observer)
            .map_err(NativeMarkdownPagedCellCommitError::Commit)?;
        let outcome = match result {
            Some((edit, mapped)) => {
                // canonical已经发布。publish只交换原FoldObserver的有限metadata与selection，
                // 没有current、Native或可失败IO；receipt始终属于原Body而非局部Cell坐标域。
                NativeMarkdownPagedInputOutcome::Committed(body.publish(
                    body_permit,
                    document.generation(),
                    mapped,
                    None,
                    Some(edit),
                    &mut folds,
                ))
            }
            None => {
                // Core原未改内容路径不运行observer、不移动canonical选择、不制造空Undo。
                // 因此不虚构PreparedFolds或强行推进Body serial，只结束原局部会话。
                NativeMarkdownPagedInputOutcome::Unchanged(body_permit.before)
            }
        };
        self.seal_committed_cell(&session);
        Ok(outcome)
    }

    /// 只在Core成功后封闭原局部metadata；不调用可失败close或后置Source读取。
    fn seal_committed_cell(&self, session: &NativeMarkdownPagedCellSession) {
        self.closed.set(true);
        self.marked.set(None);
        if self.selection.serial() != u64::MAX {
            self.selection.seal();
        }
        session.mark_committed();
    }

    /// 两个Scene读取值仍活时准备唯一准入输出；小结果使用有限堆字符串，长结果沿匿名存储封存。
    ///
    /// # Errors
    /// 原票/Source错误、marked、文档预算、候选存储失败或已有准入时拒绝；
    /// 失败不清Scene、草稿、局部历史或canonical。
    pub fn prepare_cell_commit(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedCellPreparedCommit> {
        let _mutation = self.begin(permit, current)?;
        let cell = permit
            .cell
            .as_ref()
            .ok_or(NativeMarkdownPagedInputError::BodyTargetRequired)?;
        self.check_permit(permit, current)?;
        let session = cell.session()?;
        let original = session.document()?;
        let prepared = {
            let document = original
                .try_borrow()
                .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
            document.prepare_cell_draft_commit(cell.take()?)?
        };
        session.facts()?; // Source首错先于可重入前台门；准入本身不改变原serial或原草稿。
        self.check_original(permit.epoch, permit.before, current)?;
        Ok(NativeMarkdownPagedCellPreparedCommit {
            owner: Rc::downgrade(self),
            session: Rc::downgrade(&session),
            epoch: permit.epoch,
            before: permit.before,
            prepared,
        })
    }

    /// 控制器真实撤掉两个Scene后，消费同张准入票执行一次canonical发布；不重prepare/重sanitize。
    ///
    /// # Errors
    /// 原目标/serial失效、Reader仍活、Source或观察者失败时保留原草稿；准入输出按实际Drop归还。
    pub fn commit_cell_observed<O: MarkdownTextChangeObserver>(
        self: &Rc<Self>,
        prepared: NativeMarkdownPagedCellPreparedCommit,
        observer: O,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> NativeMarkdownPagedCellCommitResult<O> {
        if !Weak::ptr_eq(&prepared.owner, &Rc::downgrade(self)) {
            return Err(NativeMarkdownPagedInputError::Revoked.into());
        }
        let session = prepared
            .session
            .upgrade()
            .ok_or(NativeMarkdownPagedInputError::Closed)?;
        session.facts()?;
        self.check_original(prepared.epoch, prepared.before, current)?;
        if self.busy.replace(true) {
            return Err(NativeMarkdownPagedInputError::Busy.into());
        }
        let _mutation = Mutation(&self.busy);
        let original = session.document()?;
        let mut document = original
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        let result = document
            .commit_prepared_cell_draft_observed(prepared.prepared, observer)
            .map_err(NativeMarkdownPagedCellCommitError::Commit)?;
        // canonical已经提交或确认为原未改内容；此后仅封闭旧Cell metadata，绝不执行可失败的后置读取。
        self.seal_committed_cell(&session);
        Ok(result)
    }
}
