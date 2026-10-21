//! Local input changes an independent descriptor branch. Replace the draft root
//! and serial only after every fallible preparation and validation succeeds.

use super::*;

/// Fixed nonce and serial for one local edit. Only the original session may
/// capture it; it cannot be cloned or constructed from bare coordinates.
pub struct MarkdownCellDraftEditCapsule {
    scope: Weak<Scope>,
    serial: u64,
}

pub(super) struct DraftSource<'a> {
    pub(super) document: &'a MarkdownDocumentBuffer,
    pub(super) edits: &'a MarkdownEditSegments,
    pub(super) scope: &'a Scope,
    pub(super) serial: u64,
}

impl PieceByteSource for DraftSource<'_> {
    fn source_health(&self) -> Result<(), MarkdownDocumentBufferError> {
        // Original storage and draft deltas retain their own actual health before
        // serial revocation is considered. A delta-only tree cannot hide original I/O.
        self.document
            .original_backing_health()
            .inspect_err(|&error| self.scope.record_source(error))?;
        self.edits
            .health()
            .inspect_err(|&error| self.scope.record_source(error))?;
        self.scope
            .require(self.serial)
            .map_err(|error| match error {
                MarkdownCellDraftError::Document(error) => error,
                // Tree internals return Core errors. The original outer token
                // boundary still distinguishes draft authority with its own Stale.
                _ => MarkdownDocumentBufferError::PreparedTextInputStale,
            })
    }

    fn piece_bytes(
        &self,
        piece: &MarkdownPiece,
    ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError> {
        self.source_health()?;
        match piece.source {
            MarkdownPieceSource::Original => self.document.piece_bytes(piece),
            MarkdownPieceSource::EditSegment(index) => {
                let end = piece
                    .byte_start
                    .checked_add(piece.byte_len)
                    .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
                self.edits.read(index, piece.byte_start..end)
            }
        }
    }

    /// Keep this same draft scope/segment table and pass only this call's original
    /// leaf-local byte range to actual storage.
    #[cfg(feature = "paged-shaping-frontier")]
    fn piece_range_bytes(
        &self,
        piece: &MarkdownPiece,
        local: Range<usize>,
    ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError> {
        self.source_health()?;
        if local.start > local.end || local.end > piece.byte_len {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        match piece.source {
            MarkdownPieceSource::Original => self.document.piece_range_bytes(piece, local),
            MarkdownPieceSource::EditSegment(index) => self.edits.read(
                index,
                piece.byte_start + local.start..piece.byte_start + local.end,
            ),
        }
    }
}

pub(super) fn validate_positions(
    source: &DraftSource<'_>,
    root: &PieceTreeLink,
    selection: MarkdownTextSelection,
    marked: &Option<Range<usize>>,
) -> Result<(), MarkdownDocumentBufferError> {
    for at in [selection.anchor_utf16, selection.head_utf16] {
        // Selection cannot lie inside a surrogate pair. The same actual tree
        // lookup rejects endpoints beyond its real extent.
        byte_offset_for_utf16_in_tree(source, root.as_deref(), at, 0)?;
    }
    if let Some(range) = marked {
        // Marked ranges have independent directed bounds: validate both scalar
        // endpoints, rather than accepting a length measured against another root.
        if range.start > range.end {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        for at in [range.start, range.end] {
            byte_offset_for_utf16_in_tree(source, root.as_deref(), at, 0)?;
        }
    }
    Ok(())
}

impl PendingCellDraft {
    fn replace(
        &mut self,
        document: &MarkdownDocumentBuffer,
        range: Range<usize>,
        text: &str,
        selection: MarkdownTextSelection,
        marked: Option<Range<usize>>,
        kind: Option<MarkdownCellDraftInputKind>,
    ) -> Result<MarkdownCellDraftFacts, MarkdownCellDraftError> {
        self.require(document)?;
        if range.start > range.end {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument.into());
        }
        let source = DraftSource {
            document,
            edits: &self.edits,
            scope: &self.scope,
            serial: self.serial,
        };
        let start = byte_offset_for_utf16_in_tree(&source, self.root.as_deref(), range.start, 0)?;
        let end = byte_offset_for_utf16_in_tree(&source, self.root.as_deref(), range.end, 0)?;
        if range.is_empty() && text.is_empty() {
            if let Some(kind) = kind {
                history::grouping::validate_input_kind(kind, range.start, 0, &marked)?;
            }
            // No character change goes through the state-only boundary. Selection
            // or unmark cannot create history roots or revoke an unchanged text Source.
            return self.set_state(document, selection, marked);
        }
        let next_serial = self
            .serial
            .checked_add(1)
            .ok_or(MarkdownCellDraftError::Stale)?;
        let next_content_serial = self
            .content_serial
            .checked_add(1)
            .ok_or(MarkdownCellDraftError::Stale)?;
        let replacement =
            MarkdownReplacementMetrics::new(range.start, range.len(), text.encode_utf16().count())?;
        let group = self.history.plan_group(history::grouping::GroupChange {
            kind,
            serial: self.serial,
            next_serial,
            selection_before: self.selection,
            marked_before: &self.marked_range,
            selection_after: selection,
            marked_after: &marked,
            replacement,
        })?;
        self.history.reserve_group(&group)?;
        // Split the old candidate before appending, so an endpoint/source failure
        // creates no unused delta. Original Source failure precedes new input commit.
        let (prefix, tail) = split_piece_tree(&source, self.root.clone(), start)?;
        let (_, suffix) = split_piece_tree(&source, tail, end - start)?;
        let checkpoint = command_transaction::AppendCheckpoint::capture_segments(&self.edits);
        let result = (|| {
            let added = if text.len() > EDIT_SEGMENT_BYTES {
                let backing = self
                    .edits
                    .freeze_input(text, MARKDOWN_CELL_DRAFT_MAX_DELTA_BYTES)?;
                // Observe only the newly created raw file, before later leaf reads
                // or transaction rollback can fail. Inherited canonical segments
                // do not enter here; their final release still belongs to the document.
                self.scope.observe_raw_backing(backing.observer());
                self.edits
                    .append_frozen(backing, MARKDOWN_CELL_DRAFT_MAX_DELTA_BYTES)?
            } else {
                // A bounded heap tail has no mapping obligation. Do not allocate
                // an observer list or invent a file receipt for that path.
                self.edits
                    .append(text, MARKDOWN_CELL_DRAFT_MAX_DELTA_BYTES)?
            };
            // Each segment loses at most three UTF-8 tail bytes. A separate
            // descriptor bound prevents many scalar-sized appends from accumulating
            // an unbounded collection of tiny tail segments.
            let maximum_descriptors = self
                .base_descriptors
                .saturating_add(
                    MARKDOWN_CELL_DRAFT_MAX_DELTA_BYTES.div_ceil(EDIT_SEGMENT_BYTES - 3),
                )
                .saturating_add(1);
            if self.edits.segments.len() > maximum_descriptors {
                return Err(MarkdownDocumentBufferError::EditBufferLimitExceeded);
            }
            let root = join_piece_trees(
                prefix,
                join_piece_trees(build_balanced_piece_tree(added), suffix),
            );
            let source = DraftSource {
                document,
                edits: &self.edits,
                scope: &self.scope,
                serial: self.serial,
            };
            validate_positions(&source, &root, selection, &marked)?;
            source.source_health()?;
            self.edits.pending_health()?; // Transfer new files to raw Source only after successful preparation; failure retains the old branch.
            Ok(root)
        })();
        let root = match result {
            Ok(root) => root,
            Err(error) => {
                // The closure's failed candidate root has already dropped. Roll
                // back this branch only; canonical text and old reader segments
                // cannot be modified through the candidate's storage checkpoint.
                checkpoint.rollback_segments(&mut self.edits);
                self.scope.record_source(error);
                return Err(error.into());
            }
        };
        self.history
            .record_group(group, self.root.take(), self.selection);
        self.edits.publish(); // All validation succeeded; new anonymous segments now belong to the original raw Source.
        self.root = root;
        self.selection = selection;
        self.marked_range = marked;
        self.serial = next_serial;
        self.content_serial = next_content_serial;
        self.scope.publish_delta(self.edits.used_bytes);
        self.scope
            .content_serial
            .store(next_content_serial, Ordering::Release);
        self.scope.serial.store(next_serial, Ordering::Release);
        Ok(self.facts())
    }
}

impl MarkdownDocumentBuffer {
    pub(super) fn require_cell_draft_edit(
        &self,
        edit: &MarkdownCellDraftEditCapsule,
    ) -> Result<&PendingCellDraft, MarkdownCellDraftError> {
        let pending = self.active_cell_draft_scope(&edit.scope)?;
        if pending.serial != edit.serial {
            // A live session cannot renew old UI ranges with the current serial.
            // After rejection the caller must capture new authority explicitly.
            return Err(MarkdownCellDraftError::Stale);
        }
        Ok(pending)
    }

    /// Capture this input authority before native calls/UI reads. A later
    /// successful input-state change revokes the old receipt.
    pub fn capture_cell_draft_edit(
        &self,
        token: &MarkdownCellDraft,
    ) -> Result<MarkdownCellDraftEditCapsule, MarkdownCellDraftError> {
        let pending = self.active_cell_draft(token)?;
        Ok(MarkdownCellDraftEditCapsule {
            scope: Arc::downgrade(&pending.scope),
            serial: pending.serial,
        })
    }

    /// Replace raw input in cell-local coordinates. Large draft admission does
    /// not prematurely impose the 64 Ki-unit inline commit threshold. Failure
    /// preserves the draft, serial, local selection/marked state and canonical Undo baseline.
    pub fn replace_cell_draft(
        &mut self,
        edit: MarkdownCellDraftEditCapsule,
        range: Range<usize>,
        text: &str,
        selection_after: MarkdownTextSelection,
        marked_after: Option<Range<usize>>,
    ) -> Result<MarkdownCellDraftFacts, MarkdownCellDraftError> {
        self.require_cell_draft_edit(&edit)?;
        let Some(CellDraftSlot::Active(mut pending)) = self.cell_draft.take() else {
            unreachable!("已核唯一活动草稿");
        };
        // Temporarily take the slot so the candidate Source can borrow the document
        // during synchronous pure Rust editing. No external/native callback runs.
        let result = pending.replace(self, range, text, selection_after, marked_after, None);
        self.cell_draft = Some(CellDraftSlot::Active(pending));
        result
    }

    /// Consume the same original input's two sealed receipts. Merge only proven
    /// adjacent typing or replacement of that same complete marked range.
    /// Authority, semantic-input or actual append failure leaves the original
    /// root, grouping, delta, selection and canonical document unchanged.
    pub fn replace_cell_draft_grouped(
        &mut self,
        edit: MarkdownCellDraftEditCapsule,
        intent: MarkdownCellDraftInputIntent,
        range: Range<usize>,
        text: &str,
        selection_after: MarkdownTextSelection,
        marked_after: Option<Range<usize>>,
    ) -> Result<MarkdownCellDraftFacts, MarkdownCellDraftError> {
        let pending = self.require_cell_draft_edit(&edit)?;
        let kind = intent.require(pending)?;
        let Some(CellDraftSlot::Active(mut pending)) = self.cell_draft.take() else {
            unreachable!("已核原输入意图的唯一草稿");
        };
        let result = pending.replace(self, range, text, selection_after, marked_after, Some(kind));
        self.cell_draft = Some(CellDraftSlot::Active(pending));
        result
    }
}
