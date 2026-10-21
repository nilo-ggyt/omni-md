//! The document's unique cell-draft branch. Local coordinates, a separate nonce,
//! and delta segments never impersonate the canonical document text.

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

/// Cumulative UTF-8 delta allowance for uncommitted input, separate from the
/// canonical session's append inventory.
pub const MARKDOWN_CELL_DRAFT_MAX_DELTA_BYTES: usize = 32 * 1024 * 1024;
/// UTF-16 threshold of the bounded String sanitization path. Larger drafts use
/// the same rules with sealed file output; this is not a supported-content limit.
pub const MARKDOWN_CELL_DRAFT_COMMIT_UTF16_UNITS: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
/// Failure does not publish canonical text. Source errors keep their actual
/// typed first cause rather than becoming ordinary draft staleness.
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
/// All positions address the cell-local raw draft. canonical_range lets the
/// original owner locate it, without granting authority over another Source.
pub struct MarkdownCellDraftFacts {
    /// Advances for every actual input-state change; old edit capsules cannot
    /// renew their coordinate authority from an equal-looking successor state.
    pub serial: u64,
    /// Advances only when text/history root changes. Selection and unmark changes
    /// do not revoke the layout Source for the same text.
    pub content_serial: u64,
    pub byte_len: usize,
    pub utf16_len: usize,
    pub selection: MarkdownTextSelection,
    pub marked_range: Option<Range<usize>>,
    pub canonical_range: Range<usize>,
}

pub(super) enum CellDraftSlot {
    // One actual owner holds the active branch. Boxing avoids making an otherwise
    // empty retired slot retain the size of all local history metadata.
    Active(Box<PendingCellDraft>),
    // The cancelled branch has actually dropped. Await at most two original
    // reader payloads without retaining a candidate root or segment table here.
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
    // Field drop order is part of ownership: only after the actual root, segments,
    // plan and history above are gone may this claim mark branch_live false.
    _claim: BranchClaim,
}

impl PendingCellDraft {
    fn matches(&self, token: &MarkdownCellDraft) -> bool {
        // Only the original scope's weak nonce matches; equal serials or text in
        // another document cannot authorize cancellation of this branch.
        Weak::ptr_eq(&token.scope, &Arc::downgrade(&self.scope))
    }

    fn require(&self, doc: &MarkdownDocumentBuffer) -> Result<(), MarkdownCellDraftError> {
        // Actual original and delta storage failures precede scope revocation or
        // stale plan identity, including when the current root has no original leaf.
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
    /// Begin the unique draft from an original verified table window. Slice/share
    /// the actual cell tree without reading its whole content or building a String.
    pub fn begin_cell_draft(
        &mut self,
        window: &VerifiedMarkdownTableWindow,
        row_ordinal: usize,
        column: usize,
    ) -> Result<MarkdownCellDraft, MarkdownCellDraftError> {
        let plan = self.plan_verified_table_cell_edit(window, row_ordinal, column, "")?;
        self.begin_prepared_cell_draft(plan)
    }

    /// Short windows and long-cell requests use the same plan and unique draft
    /// slot. Only the same-source producer may issue that plan; this edit authority
    /// neither implies inline-preview Ready nor requires a complete-cell read.
    pub fn begin_prepared_cell_draft(
        &mut self,
        plan: MarkdownTableCellEditPlan,
    ) -> Result<MarkdownCellDraft, MarkdownCellDraftError> {
        self.original_backing_health()?;
        self.ensure_text_input_idle()?;
        let (range, replacement) = plan.replacement_for(self)?;
        if replacement.len() != plan.separator_prefix_bytes {
            // Public plans can also carry explicit replacement text. Activation
            // accepts only empty payload, never disguising user text as missing-cell prefix.
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
        // Clone only existing bounded descriptors, sharing actual bytes through
        // Arc. used_bytes counts new draft delta; segment numbering keeps the
        // sliced original tree readable without copying its text.
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
        // Preserve original Source failure even for a foreign token; Stale must
        // not conceal an actual mmap failure from this document's backing.
        self.original_backing_health().inspect_err(|&error| {
            match &self.cell_draft {
                Some(CellDraftSlot::Active(pending)) => pending.scope.record_source(error),
                Some(CellDraftSlot::Retired(scope)) => scope.record_source(error),
                None => {} // No draft scope can record it; the actual document failure still remains in the result.
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

    /// Read the original nonce's current local facts without borrowing text or
    /// changing the canonical selection.
    pub fn cell_draft_facts(
        &self,
        token: &MarkdownCellDraft,
    ) -> Result<MarkdownCellDraftFacts, MarkdownCellDraftError> {
        Ok(self.active_cell_draft(token)?.facts())
    }

    /// Return a fixed resource observer with no text owner. Acquiring it still
    /// requires this currently active draft's authority.
    pub fn cell_draft_resource_observer(
        &self,
        token: &MarkdownCellDraft,
    ) -> Result<MarkdownCellDraftResourceObserver, MarkdownCellDraftError> {
        Ok(MarkdownCellDraftResourceObserver {
            scope: Arc::clone(&self.active_cell_draft(token)?.scope),
        })
    }

    /// Explicit cancellation revokes only the draft, without publishing text or
    /// recording Undo. An already-failed Source cannot prevent branch cleanup.
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

    /// Drain a branch cancelled by token Drop or Escape. Keep the unique retired
    /// slot while actual reader payloads or prepared-commit storage remain owned.
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
                // Drop the actual root and branch segments before deciding which
                // retired claims remain. Do not clear counters in place of releasing owners.
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
    /// Include the retired slot awaiting actual readers/prepared commit. That slot
    /// still excludes concurrent canonical writes until its owners are released.
    pub fn has_cell_draft(&self) -> bool {
        self.cell_draft.is_some()
    }
}

#[cfg(test)]
mod tests;
