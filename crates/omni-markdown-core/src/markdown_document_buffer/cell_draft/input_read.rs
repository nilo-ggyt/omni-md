//! Synchronous bounded reads through the original input capsule. Do not freeze a
//! new root or occupy either of the two actual layout-reader slots.

use super::edit::DraftSource;
use super::*;

impl MarkdownDocumentBuffer {
    /// Check the original serial and return local state plus weak text identity.
    /// A selection-state version does not impersonate the content version.
    ///
    /// # Errors
    /// Reject original Source failure, cancellation, nonce or serial mismatch,
    /// without recapturing authority from a successor state.
    pub fn cell_draft_edit_facts(
        &self,
        edit: &MarkdownCellDraftEditCapsule,
    ) -> Result<(MarkdownCellDraftFacts, MarkdownCellDraftSourceAuthority), MarkdownCellDraftError>
    {
        let pending = self.require_cell_draft_edit(edit)?;
        let facts = pending.facts();
        let authority = guard::source_authority(&pending.scope, facts.content_serial);
        self.require_cell_draft_edit(edit)?; // Recheck before returning; cached facts cannot hide an observed background failure.
        Ok((facts, authority))
    }

    /// Read at most 4096 UTF-16 units per call, permitting a surrogate-half start.
    /// Reuse only the document's existing single 8 KiB derived window.
    ///
    /// # Errors
    /// Limits, invalid bounds, revoked original authority or Source failure return
    /// an error. The caller must discard the complete candidate already written.
    pub fn copy_cell_draft_utf16_code_units(
        &self,
        edit: &MarkdownCellDraftEditCapsule,
        offset: usize,
        destination: &mut [u16],
    ) -> Result<(), MarkdownCellDraftError> {
        let pending = self.require_cell_draft_edit(edit)?;
        if destination.len() > read_cache::READ_WINDOW_UTF16_UNITS {
            return Err(MarkdownCellDraftError::ReadLimit); // Check original Source before rejecting an unbounded native request.
        }
        let source = DraftSource {
            document: self,
            edits: &pending.edits,
            scope: &pending.scope,
            serial: pending.serial,
        };
        let result = self
            .utf16_read_cache
            .lock()
            .map_err(|_| MarkdownDocumentBufferError::Utf16ReadCacheUnavailable)
            .and_then(|mut cache| {
                // Actual weak-root identity and content_serial isolate canonical
                // text and other drafts. Cache hits still check Source health.
                cache.copy(
                    &source,
                    &pending.root,
                    pending.content_serial,
                    offset,
                    destination,
                )
            });
        self.finish_cell_draft_input_read(edit, pending, result)
    }

    /// Validate actual scalar endpoints of a draft-local range, without exporting
    /// canonical coordinates or granting another editing permission.
    ///
    /// # Errors
    /// Reject reversed, overflowing, out-of-range or surrogate-interior bounds,
    /// as well as original Source/authority failure.
    pub fn validate_cell_draft_utf16_range(
        &self,
        edit: &MarkdownCellDraftEditCapsule,
        range: Range<usize>,
    ) -> Result<(), MarkdownCellDraftError> {
        let pending = self.require_cell_draft_edit(edit)?;
        let source = DraftSource {
            document: self,
            edits: &pending.edits,
            scope: &pending.scope,
            serial: pending.serial,
        };
        let result = (|| {
            if range.start > range.end {
                return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
            }
            for at in [range.start, range.end] {
                byte_offset_for_utf16_in_tree(&source, pending.root.as_deref(), at, 0)?;
            } // Check only two endpoint paths; UTF-16 distance is not a grapheme/glyph count or a reason to scan the range.
            Ok(())
        })();
        self.finish_cell_draft_input_read(edit, pending, result)
    }

    fn finish_cell_draft_input_read<T>(
        &self,
        edit: &MarkdownCellDraftEditCapsule,
        pending: &PendingCellDraft,
        result: Result<T, MarkdownDocumentBufferError>,
    ) -> Result<T, MarkdownCellDraftError> {
        if let Err(error) = &result {
            pending.scope.record_source(*error); // Actual errno stays sticky; staleness or later success cannot replace the first cause.
        }
        self.require_cell_draft_edit(edit)?; // The cache lock is gone; empty ranges/EOF still require original health and serial.
        result.map_err(Into::into)
    }
}
