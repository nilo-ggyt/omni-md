//! 同Doc的唯一单元格草稿分支；局部坐标、独立nonce和新增段不冒充canonical正文。

use std::sync::Weak;
use std::sync::atomic::Ordering;

use super::table_cell_edit::MarkdownTableCellEditPlan;
use super::*;
use crate::markdown_viewport_presentation::table_window::MarkdownTableWindowError;

mod commit;
mod edit;
mod formatting;
mod guard;
mod history;
mod input_read;
mod read;
mod replacement;
mod resources;
mod state;
pub use commit::{
    MarkdownCellDraftCommit, MarkdownCellDraftCommitError, MarkdownCellDraftCommitReceipt,
    MarkdownCellDraftCommitResult, MarkdownCellDraftPreparedCommit, MarkdownCellDraftWordAnchor,
};
pub use edit::MarkdownCellDraftEditCapsule;
pub(super) use guard::FrozenDraftGuard;
pub use guard::MarkdownCellDraftSourceAuthority;
pub use history::{
    MarkdownCellDraftHistoryDirection, MarkdownCellDraftHistoryEdit, MarkdownCellDraftHistoryFacts,
    MarkdownCellDraftInputIntent, MarkdownCellDraftInputKind,
};
pub use read::{MarkdownCellDraftReadAuthority, MarkdownCellDraftReadCapsule};
pub(super) use resources::ReadClaim;
use resources::{BranchClaim, Scope};
pub use resources::{
    MarkdownCellDraft, MarkdownCellDraftResourceObserver, MarkdownCellDraftResourceSnapshot,
};

/// 未提交输入的累计新增UTF-8预算；不占用canonical的会话append库存。
pub const MARKDOWN_CELL_DRAFT_MAX_DELTA_BYTES: usize = 32 * 1024 * 1024;
/// 原有限String清洗快路的UTF16阈值；更大的草稿通过同规则写入封存文件，不是功能上限。
pub const MARKDOWN_CELL_DRAFT_COMMIT_UTF16_UNITS: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
/// 失败不发布canonical；Source错误仍保真实typed首因。
pub enum MarkdownCellDraftError {
    #[error(transparent)]
    Document(#[from] MarkdownDocumentBufferError),
    #[error(transparent)]
    Table(#[from] MarkdownTableWindowError),
    #[error("当前单元格草稿权限已失效")]
    Stale,
    #[error("单元格草稿仍有读取值，尚不能归还或提交")]
    ReadersBusy,
    #[error("单元格草稿的单次读取超过有限窗口")]
    ReadLimit,
    #[error("单元格提交超过当前文档或会话容量")]
    CommitLimit,
    #[error("单元格已有提交准入票，请先完成或取消本次提交")]
    CommitBusy,
    #[error("请先完成当前单元格输入法组合")]
    CompositionActive,
    #[error("单元格输入意图与原始组合范围不符")]
    InvalidInputIntent,
    #[error("该格式需要正文块位置")]
    FormatRequiresDocument,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// 位置都属于cell局部原始草稿；canonical_range仅供原owner定位，不授予另一Source权限。
pub struct MarkdownCellDraftFacts {
    /// 任一实际输入状态改变都会推进；旧编辑胶囊不得重新补签坐标。
    pub serial: u64,
    /// 只有正文或历史root改变才推进；选区与unmark不撤销同正文的布局来源。
    pub content_serial: u64,
    pub byte_len: usize,
    pub utf16_len: usize,
    pub selection: MarkdownTextSelection,
    pub marked_range: Option<Range<usize>>,
    pub canonical_range: Range<usize>,
}

pub(super) enum CellDraftSlot {
    // 活动分支只有一个实际owner；Box避免retired空槽也携带整份历史metadata体积。
    Active(Box<PendingCellDraft>),
    // 已取消分支已真Drop；只等两个以内原读取payload归还，不持候选树或段表。
    Retired(Arc<Scope>),
}

pub(super) struct PendingCellDraft {
    root: PieceTreeLink,
    edits: MarkdownEditSegments,
    plan: MarkdownTableCellEditPlan,
    range: Range<usize>,
    selection: MarkdownTextSelection,
    marked_range: Option<Range<usize>>,
    serial: u64,
    content_serial: u64,
    base_descriptors: usize,
    scope: Arc<Scope>,
    history: history::LocalHistory,
    // 按字段Drop顺序，只有上面的真实树、段与plan结束后才签branch_live=false。
    _claim: BranchClaim,
}

impl PendingCellDraft {
    fn matches(&self, token: &MarkdownCellDraft) -> bool {
        Weak::ptr_eq(&token.scope, &Arc::downgrade(&self.scope))
    }

    fn require(&self, doc: &MarkdownDocumentBuffer) -> Result<(), MarkdownCellDraftError> {
        doc.original_backing_health()
            .inspect_err(|&error| self.scope.record_source(error))?;
        self.edits
            .health()
            .inspect_err(|&error| self.scope.record_source(error))?;
        self.scope.require(self.serial)?;
        self.plan.replacement_for(doc)?;
        Ok(())
    }

    fn facts(&self) -> MarkdownCellDraftFacts {
        let metrics = piece_tree_metrics(&self.root);
        MarkdownCellDraftFacts {
            serial: self.serial,
            content_serial: self.content_serial,
            byte_len: metrics.byte_len,
            utf16_len: metrics.utf16_len,
            selection: self.selection,
            marked_range: self.marked_range.clone(),
            canonical_range: self.range.clone(),
        }
    }
}

impl MarkdownDocumentBuffer {
    /// 从原表格认证窗口取得唯一草稿；原cell树直接切出共享，不读取整格或构造String。
    pub fn begin_cell_draft(
        &mut self,
        window: &VerifiedMarkdownTableWindow,
        row_ordinal: usize,
        column: usize,
    ) -> Result<MarkdownCellDraft, MarkdownCellDraftError> {
        let plan = self.plan_verified_table_cell_edit(window, row_ordinal, column, "")?;
        self.begin_prepared_cell_draft(plan)
    }

    /// 短窗口与原有长格请求共用原plan和唯一草稿槽；计划只能由同源producer生成。
    /// 不把这个编辑权限解释为inline预览Ready，也不读取完整cell。
    pub fn begin_prepared_cell_draft(
        &mut self,
        plan: MarkdownTableCellEditPlan,
    ) -> Result<MarkdownCellDraft, MarkdownCellDraftError> {
        self.original_backing_health()?;
        self.ensure_text_input_idle()?;
        let (range, replacement) = plan.replacement_for(self)?;
        if replacement.len() != plan.separator_prefix_bytes {
            // 原公开plan也可承载一次显式替换；激活只接受空正文，不能把用户替换文本当补格前缀。
            return Err(MarkdownTableWindowError::InvalidSource.into());
        }
        let range = range.clone();
        let byte_start = self.byte_offset_for_utf16(range.start)?;
        let byte_end = self.byte_offset_for_utf16(range.end)?;
        let (_, tail) = split_piece_tree(self, self.root.clone(), byte_start)?;
        let (root, _) = split_piece_tree(self, tail, byte_end - byte_start)?;
        let scope = Scope::new();
        let token = MarkdownCellDraft {
            scope: Arc::downgrade(&scope),
        };
        // 只复制原预算内descriptor；原字节共享Arc。used_bytes专计新delta，编号保持原树可读。
        let edits = MarkdownEditSegments {
            segments: self.edits.segments.clone(),
            used_bytes: 0,
            published_segments: self.edits.published_segments,
        };
        let base_descriptors = edits.segments.len();
        self.cell_draft = Some(CellDraftSlot::Active(Box::new(PendingCellDraft {
            root,
            edits,
            plan,
            range,
            selection: MarkdownTextSelection::default(),
            marked_range: None,
            serial: 0,
            content_serial: 0,
            base_descriptors,
            scope: Arc::clone(&scope),
            history: history::LocalHistory::default(),
            _claim: BranchClaim(scope),
        })));
        Ok(token)
    }

    fn active_cell_draft(
        &self,
        token: &MarkdownCellDraft,
    ) -> Result<&PendingCellDraft, MarkdownCellDraftError> {
        self.active_cell_draft_scope(&token.scope)
    }

    fn active_cell_draft_scope(
        &self,
        scope: &Weak<Scope>,
    ) -> Result<&PendingCellDraft, MarkdownCellDraftError> {
        // 即使外来token也先保原Source错误；不能让Stale遮住实际mmap失败。
        self.original_backing_health().inspect_err(|&error| {
            match &self.cell_draft {
                Some(CellDraftSlot::Active(pending)) => pending.scope.record_source(error),
                Some(CellDraftSlot::Retired(scope)) => scope.record_source(error),
                None => {} // 没有原草稿scope可记录；真实Doc首错仍沿返回值保留。
            }
        })?;
        let Some(CellDraftSlot::Active(pending)) = &self.cell_draft else {
            return Err(MarkdownCellDraftError::Stale);
        };
        if !Weak::ptr_eq(scope, &Arc::downgrade(&pending.scope)) {
            return Err(MarkdownCellDraftError::Stale);
        }
        pending.require(self)?;
        Ok(pending)
    }

    /// 只读原nonce当前局部事实，不借入正文或改变canonical选区。
    pub fn cell_draft_facts(
        &self,
        token: &MarkdownCellDraft,
    ) -> Result<MarkdownCellDraftFacts, MarkdownCellDraftError> {
        Ok(self.active_cell_draft(token)?.facts())
    }

    /// 返回固定无owner资源票；取得它本身仍须属于当前草稿。
    pub fn cell_draft_resource_observer(
        &self,
        token: &MarkdownCellDraft,
    ) -> Result<MarkdownCellDraftResourceObserver, MarkdownCellDraftError> {
        Ok(MarkdownCellDraftResourceObserver {
            scope: Arc::clone(&self.active_cell_draft(token)?.scope),
        })
    }

    /// 显式取消只撤草稿；不发布正文、不记录Undo，也不因Source已失败而阻止释放。
    pub fn cancel_cell_draft(&mut self, token: &MarkdownCellDraft) -> bool {
        let Some(CellDraftSlot::Active(pending)) = &self.cell_draft else {
            return false;
        };
        if !pending.matches(token) {
            return false;
        }
        pending.scope.cancelled.store(true, Ordering::Release);
        self.drain_cancelled_cell_draft();
        true
    }

    /// 排空被token Drop或Esc撤销的分支；读取payload或准入String未真Drop时保持唯一retired槽。
    pub fn drain_cancelled_cell_draft(&mut self) -> bool {
        let releasable = match &self.cell_draft {
            Some(CellDraftSlot::Active(pending)) => pending.scope.cancelled.load(Ordering::Acquire),
            Some(CellDraftSlot::Retired(scope)) => !scope.has_retired_owners(),
            None => false,
        };
        if !releasable {
            return false;
        }
        let slot = self.cell_draft.take().expect("已检查唯一草稿slot");
        match slot {
            CellDraftSlot::Active(pending) => {
                let scope = Arc::clone(&pending.scope);
                // 真正丢弃root与分支段表后才决定是否保留retired账目；不是先把计数清零。
                drop(pending);
                if scope.has_retired_owners() {
                    self.cell_draft = Some(CellDraftSlot::Retired(scope));
                }
            }
            CellDraftSlot::Retired(_) => {}
        }
        true
    }

    #[must_use]
    /// 包含等待真实读取payload或提交准入归还的retired槽；它仍禁止并行canonical写入。
    pub fn has_cell_draft(&self) -> bool {
        self.cell_draft.is_some()
    }
}

#[cfg(test)]
mod tests;
