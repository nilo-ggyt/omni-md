//! One original cell Source issued by the actual table producer. Document
//! admission may turn its structural receipt into a draft plan, never inline Ready.

use super::table_cell_edit::TableCellEditContext;
use super::table_source::MarkdownTableSourceIdentity;
use super::*;
use crate::markdown_viewport_presentation::table_window::MarkdownTableWindowCellSource;

#[derive(Clone, Debug)]
/// One receipt binds weak source-root identity, actual row/column and original
/// endpoints. It retains no text, active document, or strong native reference.
pub struct VerifiedMarkdownTableCellSource {
    pub(super) source: MarkdownTableSourceIdentity,
    pub(super) context: TableCellEditContext,
    pub(super) range: Range<usize>,
    pub(super) implicit_empty: bool,
    /// The actual Scanner or bounded window proved this missing-separator count.
    /// A zero-length range alone cannot distinguish a present empty cell from a missing cell.
    pub(super) separators: usize,
    /// Column alignment sealed by the same separator scan or bounded Parser
    /// projection. Empty cells retain the same original alignment fact.
    pub(super) alignment: crate::MarkdownTableAlignment,
    pub(super) inline_seed:
        Option<std::sync::Arc<pulldown_cmark::global_frontier::VerifiedInlineSemanticSeed>>,
}

impl VerifiedMarkdownTableCellSource {
    /// Preserve actual Source failure even for empty cells. Equal length, another
    /// document, or an old root restored by Undo cannot renew this receipt.
    pub(super) fn require(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<(), MarkdownTableRequestError> {
        document
            .original_backing_health()
            .map_err(MarkdownTableRequestError::Source)?;
        if !self.source.is_current(document) {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        Ok(())
    }

    /// Return only the original cell's canonical range. A missing cell's empty
    /// insertion anchor does not by itself authorize text reads or adding pipes.
    pub fn canonical_range_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<&Range<usize>, MarkdownTableRequestError> {
        self.require(document)?;
        Ok(&self.range)
    }

    /// Preserve the Parser's distinction between present empty and implicit
    /// missing cells; identical empty display does not imply identical writes.
    pub fn source_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<MarkdownTableWindowCellSource, MarkdownTableRequestError> {
        self.require(document)?;
        Ok(if self.implicit_empty {
            MarkdownTableWindowCellSource::ImplicitEmpty {
                insertion_anchor_utf16: self.range.start,
            }
        } else {
            MarkdownTableWindowCellSource::Present(self.range.clone())
        })
    }

    /// Header position and absolute row/column come from the same structural
    /// producer. A cell's Vec position in an offscreen window supplies no identity.
    pub fn coordinate_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<(usize, usize, usize), MarkdownTableRequestError> {
        self.require(document)?;
        Ok((
            self.context.header.start,
            self.context.row_ordinal,
            self.context.column,
        ))
    }

    /// Alignment lives with this original root/cell receipt. Another same-byte
    /// document, an Undo-restored old root, or a failed Source cannot reuse it.
    ///
    /// # Errors
    /// Return the original Source failure or revoked root identity. A caller's
    /// column number cannot manufacture alignment evidence.
    pub fn alignment_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<crate::MarkdownTableAlignment, MarkdownTableRequestError> {
        self.require(document)?;
        Ok(self.alignment)
    }

    /// A same-source raw-cell semantic seed is not style Ready. An ordinary
    /// published window keeps querying its original complete semantic runs.
    pub fn inline_seed_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<
        Option<&pulldown_cmark::global_frontier::VerifiedInlineSemanticSeed>,
        MarkdownTableRequestError,
    > {
        self.require(document)?;
        Ok(self.inline_seed.as_deref())
    }
}

impl MarkdownDocumentBuffer {
    /// Consume the producer's complete cell receipt to start a draft plan. Accept
    /// no bare range, reread no whole original cell, and start no second structure request.
    ///
    /// A present empty cell replaces its existing empty range; an implicit cell
    /// adds only separators proved by the Scanner. The plan retains weak root
    /// identity. begin_prepared_cell_draft owns actual mmap/piece sharing and the
    /// unique active draft slot.
    ///
    /// # Errors
    /// Reject original storage failure, another document, or stale source receipts.
    /// Preserve typed separator-budget failure without creating a draft.
    pub fn plan_verified_table_cell_draft(
        &self,
        cell: VerifiedMarkdownTableCellSource,
    ) -> Result<MarkdownTableCellEditPlan, MarkdownTableRequestError> {
        cell.require(self)?; // Source failure precedes staleness; equal length or Undo cannot renew original authority.
        let plan = self
            .plan_table_cell_source_replacement(cell, "")
            .map_err(MarkdownTableRequestError::Window)?;
        plan.cell.require(self)?;
        Ok(plan)
    }

    /// Only an existing verified window supplies an ordinary cell Source. This
    /// accepts no bare range and does not create the unique active draft.
    pub fn verified_table_cell_source(
        &self,
        window: &VerifiedMarkdownTableWindow,
        row_ordinal: usize,
        column: usize,
    ) -> Result<VerifiedMarkdownTableCellSource, MarkdownTableRequestError> {
        self.original_backing_health()
            .map_err(MarkdownTableRequestError::Source)?;
        let presentation = window.presentation_for(self)?;
        let prepared = self.projected_table_cell_source(presentation, row_ordinal, column);
        self.original_backing_health()
            .map_err(MarkdownTableRequestError::Source)?;
        let mut cell = prepared.map_err(MarkdownTableRequestError::Window)?;
        cell.context.end = window.table_end_for(self)?.cloned();
        cell.require(self)?;
        Ok(cell)
    }

    /// Split original piece paths around this raw cell only. Bytes stay shared
    /// with original mappings/edit segments, and reads retain the layout quantum.
    #[cfg(any(
        feature = "paged-graphemes",
        feature = "paged-line-breaks",
        feature = "paged-shaping"
    ))]
    pub(crate) fn table_cell_layout_snapshot(
        &self,
        cell: &VerifiedMarkdownTableCellSource,
    ) -> Result<MarkdownTextSnapshot, MarkdownTableRequestError> {
        cell.require(self)?;
        let byte_start = self
            .byte_offset_for_utf16(cell.range.start)
            .map_err(MarkdownTableRequestError::Source)?;
        let byte_end = self
            .byte_offset_for_utf16(cell.range.end)
            .map_err(MarkdownTableRequestError::Source)?;
        let (_, tail) = split_piece_tree(self, self.root.clone(), byte_start)
            .map_err(MarkdownTableRequestError::Source)?;
        let (root, _) = split_piece_tree(self, tail, byte_end - byte_start)
            .map_err(MarkdownTableRequestError::Source)?;
        let snapshot = MarkdownTextSnapshot::capture_table_cell_root(self, &root);
        cell.require(self)?; // Successful freezing cannot conceal failure while returning an actual endpoint lease.
        Ok(snapshot)
    }
}
