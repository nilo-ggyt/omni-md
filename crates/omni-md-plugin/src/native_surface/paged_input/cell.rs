//! The sole Cell session owner; Input/View borrow weakly, and draft text becomes neither a second document nor TextKit storage.

use super::*;
use omni_markdown_core::markdown_document_buffer::{
    MarkdownTableCellEditPlan, VerifiedMarkdownTableCellSource,
};
use omni_markdown_core::{
    MarkdownCellDraft, MarkdownCellDraftEditCapsule, MarkdownCellDraftFacts,
    MarkdownCellDraftHistoryFacts, MarkdownCellDraftInputIntent, MarkdownCellDraftInputKind,
    MarkdownCellDraftResourceObserver, MarkdownCellDraftSourceAuthority, MarkdownLayoutTextSource,
};

/// The controller owns one draft token from a verified same-Source plan; revoke Scene before explicit close and resource verification.
pub struct NativeMarkdownPagedCellSession {
    document: Weak<RefCell<MarkdownDocumentBuffer>>,
    token: MarkdownCellDraft,
    // Captured with the original cell authority: local text edits do not change its table-column alignment.
    alignment: omni_markdown_core::MarkdownTableAlignment,
    closed: Cell<bool>,
}

impl NativeMarkdownPagedCellSession {
    /// Begin a local session from verified Cell Display/RequiredRow authority without reading the entire cell.
    ///
    /// # Errors
    /// Reject invalid original document/cell Source or unavailable unique draft slot without changing canonical text.
    pub fn begin(
        document: Weak<RefCell<MarkdownDocumentBuffer>>,
        source: VerifiedMarkdownTableCellSource,
    ) -> Result<Rc<Self>> {
        let original = document
            .upgrade()
            .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
        let plan = original
            .try_borrow()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?
            .plan_verified_table_cell_draft(source)
            .map_err(NativeMarkdownPagedInputError::CellSource)?;
        drop(original);
        Self::begin_prepared(document, plan)
    }

    /// Short-window and 4 KiB long-cell requests supply the same Core plan; it retains no text and does not substitute editing authority for a complete inline preview.
    ///
    /// # Errors
    /// Reject invalid document/Source/plan identity or unavailable draft slot while retaining canonical text and existing reads.
    pub fn begin_prepared(
        document: Weak<RefCell<MarkdownDocumentBuffer>>,
        plan: MarkdownTableCellEditPlan,
    ) -> Result<Rc<Self>> {
        let original = document
            .upgrade()
            .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
        let (token, alignment) = {
            let mut document = original
                .try_borrow_mut()
                .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
            let alignment = plan
                .alignment_for(&document)
                .map_err(NativeMarkdownPagedInputError::CellSource)?;
            (document.begin_prepared_cell_draft(plan)?, alignment)
        };
        drop(original); // Retain only a weak document identity; the plan owns neither text nor a Native graph.
        Ok(Rc::new(Self {
            document,
            token,
            alignment,
            closed: Cell::new(false),
        }))
    }

    /// Only the verified TargetDomain captures alignment; raw column indexes cannot reconstruct its authority.
    pub(super) fn alignment(&self) -> omni_markdown_core::MarkdownTableAlignment {
        self.alignment
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
        } // Core checks Source first, so Native closed metadata cannot replace an existing EIO with Closed.
        Ok(facts)
    }

    pub(super) fn mark_committed(&self) {
        self.closed.set(true); // Record only after Core publication and actual draft-branch Drop; do not certify release early.
    }

    fn history_facts(&self) -> Result<MarkdownCellDraftHistoryFacts> {
        let original = self.document()?;
        let document = original
            .try_borrow()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        let facts = document.cell_draft_history_facts(&self.token)?;
        if self.closed.get() {
            return Err(NativeMarkdownPagedInputError::Closed);
        } // Preserve Core Source priority as facts does; inspect the real local stack without falling back to canonical history.
        Ok(facts)
    }

    pub(super) fn capture(self: &Rc<Self>) -> Result<CellPermit> {
        // Core captures an edit capsule under the original draft token. Native
        // retains only a weak session and that bounded, consumable authority.
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

    /// Only Core's controlled draft factory creates layout Source; capture consumes one of the original two reader slots.
    ///
    /// # Errors
    /// Reject invalid Source/nonce, occupied reader slots, or closure; canonical Source cannot acquire draft authority here.
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

    /// This fixed atomic observer retains no document, draft root, Scene, or Native owner.
    ///
    /// # Errors
    /// Acquire while the original session is valid; an already acquired observer can verify actual Drop after closure.
    pub fn resource_observer(&self) -> Result<MarkdownCellDraftResourceObserver> {
        let original = self.document()?;
        let document = original
            .try_borrow()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        Ok(document.cell_draft_resource_observer(&self.token)?)
    }

    /// Esc cancels this candidate without canonical publication; Core retains its retired slot until actual readers return.
    ///
    /// # Errors
    /// A document borrow conflict fails explicitly instead of certifying a live branch as released; repeated close only drains original retired work again.
    pub fn close(&self) -> Result<()> {
        let original = self.document()?;
        let mut document = original
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        // Cancellation revokes the draft immediately; drain releases only branches
        // whose actual readers returned. The resource observer reports the rest.
        document.cancel_cell_draft(&self.token);
        document.drain_cancelled_cell_draft();
        self.closed.set(true);
        Ok(())
    }
}

impl Drop for NativeMarkdownPagedCellSession {
    fn drop(&mut self) {
        // Drop cannot manufacture a successful receipt: if borrowing fails, token Drop revokes its scope while document active/retired facts still block closure.
        if let Some(original) = self.document.upgrade()
            && let Ok(mut document) = original.try_borrow_mut()
        {
            document.cancel_cell_draft(&self.token);
            document.drain_cancelled_cell_draft();
        }
    }
}

impl NativeMarkdownPagedInput {
    /// Inspect actual local Cell history; Body returns None instead of silently querying canonical history.
    /// Marked input retains stack facts; menus combine them with state.marked, and history execution also rejects active composition.
    ///
    /// # Errors
    /// Reject invalid Input/document/Source/session rather than disguise closure or borrowing failure as an empty stack.
    pub fn cell_history_facts(&self) -> Result<Option<MarkdownCellDraftHistoryFacts>> {
        self.state()?;
        self.target
            .cell()?
            .map(|session| session.history_facts())
            .transpose()
    }
}

/// One Native permit captures one Core edit capsule; bounded reads borrow it, and a mutation consumes it by value.
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
        apply(&document, edit) // This closure performs bounded Core reads only, with no Native/current or other reentrant external calls.
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
        // Move the unique edit capsule into Core; no clone or successor capture
        // can turn a consumed Native callback into another mutation attempt.
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
