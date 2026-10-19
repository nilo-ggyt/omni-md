//! 原单元格的唯一会话owner；输入与视图只借Weak，局部正文不成为第二个Doc或TextKit字符串。

use super::*;
use omni_markdown_core::markdown_document_buffer::{
    MarkdownTableCellEditPlan, VerifiedMarkdownTableWindow,
};
use omni_markdown_core::{
    MarkdownCellDraft, MarkdownCellDraftEditCapsule, MarkdownCellDraftFacts,
    MarkdownCellDraftInputIntent, MarkdownCellDraftInputKind, MarkdownCellDraftResourceObserver,
    MarkdownCellDraftSourceAuthority, MarkdownLayoutTextSource,
};

/// 控制器只持同源计划生成的唯一草稿token；退出先撤Scene，再显式close并检查原资源票。
pub struct NativeMarkdownPagedCellSession {
    document: Weak<RefCell<MarkdownDocumentBuffer>>,
    token: MarkdownCellDraft,
    closed: Cell<bool>,
}

impl NativeMarkdownPagedCellSession {
    /// 从原RequiredRow生产的verified窗口开始局部会话，不读取或复制完整单元格。
    ///
    /// # Errors
    /// 原Doc、窗口/行列、Source或唯一草稿槽不再有效时拒绝，不改变canonical正文。
    pub fn begin(
        document: Weak<RefCell<MarkdownDocumentBuffer>>,
        window: Rc<VerifiedMarkdownTableWindow>,
        row_ordinal: usize,
        column: usize,
    ) -> Result<Rc<Self>> {
        let original = document
            .upgrade()
            .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
        let plan = original
            .try_borrow()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?
            .plan_verified_table_cell_edit(&window, row_ordinal, column, "")
            .map_err(omni_markdown_core::MarkdownCellDraftError::from)?;
        drop(original);
        Self::begin_prepared(document, plan)
    }

    /// 原短窗和4KiB长格请求只交同一种Core计划；不保正文、不把编辑权限当完整inline预览。
    ///
    /// # Errors
    /// 原Doc、Source、计划身份或唯一草稿槽失效时拒绝，原canonical和读取值均保留。
    pub fn begin_prepared(
        document: Weak<RefCell<MarkdownDocumentBuffer>>,
        plan: MarkdownTableCellEditPlan,
    ) -> Result<Rc<Self>> {
        let original = document
            .upgrade()
            .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
        let token = original
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?
            .begin_prepared_cell_draft(plan)?;
        drop(original); // 长期只保原Doc弱引用，计划也不保正文或Native图。
        Ok(Rc::new(Self {
            document,
            token,
            closed: Cell::new(false),
        }))
    }

    pub(super) fn document(&self) -> Result<Rc<RefCell<MarkdownDocumentBuffer>>> {
        self.document
            .upgrade()
            .ok_or(NativeMarkdownPagedInputError::DocumentReleased)
    }

    pub(super) fn facts(&self) -> Result<MarkdownCellDraftFacts> {
        let original = self.document()?;
        let document = original
            .try_borrow()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        let facts = document.cell_draft_facts(&self.token)?;
        if self.closed.get() {
            return Err(NativeMarkdownPagedInputError::Closed);
        } // Core先保真实Source首错，不能让Native关闭metadata把旧EIO改写成普通Closed。
        Ok(facts)
    }

    pub(super) fn mark_committed(&self) {
        self.closed.set(true); // 只在Core已成功发布并真实Drop原分支之后记录，不提前签资源释放。
    }

    pub(super) fn capture(self: &Rc<Self>) -> Result<CellPermit> {
        let original = self.document()?;
        let document = original
            .try_borrow()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        let edit = document.capture_cell_draft_edit(&self.token)?;
        if self.closed.get() {
            return Err(NativeMarkdownPagedInputError::Closed);
        }
        Ok(CellPermit {
            session: Rc::downgrade(self),
            edit: RefCell::new(Some(edit)),
        })
    }

    /// 只有Core受控draft factory可生产布局Source；capture占原两个读取名额之一。
    ///
    /// # Errors
    /// Source/nonce失效、两个真实reader仍占用或已关闭时拒绝，不为canonical Source补草稿票。
    pub fn capture_source(&self) -> Result<MarkdownLayoutTextSource> {
        self.facts()?;
        let original = self.document()?;
        let document = original
            .try_borrow()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        Ok(MarkdownLayoutTextSource::capture_cell_draft(
            &document,
            &self.token,
        )?)
    }

    /// 原固定原子收据不持Doc、草稿root、Scene或Native owner。
    ///
    /// # Errors
    /// 只允许在原会话仍有效时取得；关闭后仍可用已取得的observer核对真实Drop。
    pub fn resource_observer(&self) -> Result<MarkdownCellDraftResourceObserver> {
        let original = self.document()?;
        let document = original
            .try_borrow()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        Ok(document.cell_draft_resource_observer(&self.token)?)
    }

    /// Esc只取消原候选，不提交canonical；尚活的真实reader由Core保留retired槽直到归还。
    ///
    /// # Errors
    /// 原Doc正在借用时明确失败，不能把仍活分支签成已释放。重复close只再次排空原retired槽。
    pub fn close(&self) -> Result<()> {
        let original = self.document()?;
        let mut document = original
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        document.cancel_cell_draft(&self.token);
        document.drain_cancelled_cell_draft();
        self.closed.set(true);
        Ok(())
    }
}

impl Drop for NativeMarkdownPagedCellSession {
    fn drop(&mut self) {
        // 析构不能造成功收据：借用失败时token随后只撤原scope，Doc仍保retired/active事实供close门拒绝。
        if let Some(original) = self.document.upgrade()
            && let Ok(mut document) = original.try_borrow_mut()
        {
            document.cancel_cell_draft(&self.token);
            document.drain_cancelled_cell_draft();
        }
    }
}

/// 一个Native许可只捕获这一个Core编辑胶囊；有限读取借它，真正修改才按值取走。
pub(super) struct CellPermit {
    session: Weak<NativeMarkdownPagedCellSession>,
    edit: RefCell<Option<MarkdownCellDraftEditCapsule>>,
}

impl CellPermit {
    pub(super) fn with_edit<T>(
        &self,
        apply: impl FnOnce(&MarkdownDocumentBuffer, &MarkdownCellDraftEditCapsule) -> Result<T>,
    ) -> Result<T> {
        let session = self
            .session
            .upgrade()
            .ok_or(NativeMarkdownPagedInputError::Closed)?;
        let original = session.document()?;
        let document = original
            .try_borrow()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        let edit = self
            .edit
            .try_borrow()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        let edit = edit
            .as_ref()
            .ok_or(NativeMarkdownPagedInputError::Consumed)?;
        apply(&document, edit) // 回调只做有限Core读取；禁止Native/current和任何能重入的外部调用。
    }

    pub(super) fn facts(
        &self,
    ) -> Result<(MarkdownCellDraftFacts, MarkdownCellDraftSourceAuthority)> {
        self.with_edit(|document, edit| Ok(document.cell_draft_edit_facts(edit)?))
    }

    pub(super) fn intent(
        &self,
        kind: MarkdownCellDraftInputKind,
    ) -> Result<MarkdownCellDraftInputIntent> {
        self.with_edit(|document, edit| Ok(document.capture_cell_draft_input_intent(edit, kind)?))
    }

    pub(super) fn take(&self) -> Result<MarkdownCellDraftEditCapsule> {
        self.edit
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?
            .take()
            .ok_or(NativeMarkdownPagedInputError::Consumed)
    }

    pub(super) fn session(&self) -> Result<Rc<NativeMarkdownPagedCellSession>> {
        self.session
            .upgrade()
            .ok_or(NativeMarkdownPagedInputError::Closed)
    }
}
