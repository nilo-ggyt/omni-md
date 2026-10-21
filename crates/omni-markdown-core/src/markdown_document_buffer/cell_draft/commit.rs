//! Sanitize only the final draft through the existing cell-input rule, streaming
//! long results. One canonical prepare/publish provides original Undo and rollback.

use super::edit::DraftSource;
use super::replacement::{CellReplacement, CellReplacementOutput};
use super::resources::CommitClaim;
use super::*;
use changes::{MarkdownObservedEditError, MarkdownTextChangeObserver};

/// One bounded commit admission for the original input serial. It cannot be
/// cloned and retains no document, raw draft root, Source or Scene. Small results
/// own a bounded String; long results own only independent sealed storage. Public
/// ranges or strings cannot manufacture this receipt.
pub struct MarkdownCellDraftPreparedCommit {
    scope: Weak<Scope>,
    serial: u64,
    /// Only a sealed cell plan or composition with actual End supplies this range;
    /// external callers cannot populate cross-block replacement coordinates.
    range: Range<usize>,
    replacement: CellReplacement,
    replacement_utf16: usize,
    /// Directed positions mapped by this sanitization stay sealed in the candidate
    /// until actual publication or successful unchanged-content closure.
    selection: MarkdownTextSelection,
    /// Original Word-group ends or Shift anchor, mapped in the same sanitization.
    /// Commit cannot later expand a new word from an unverified numeric seed.
    word_anchor: Option<MarkdownCellDraftWordAnchor>,
    unchanged: bool,
    next_cell: Option<(usize, usize)>,
    // Field drop order releases actual candidate storage before returning the
    // unique admission slot and observing its allocation/release facts.
    _claim: CommitClaim,
}

impl MarkdownCellDraftPreparedCommit {
    #[must_use]
    /// Row/column for a new query after successful append. These coordinates
    /// neither activate the next cell nor renew an old window's Source authority.
    pub fn next_cell(&self) -> Option<(usize, usize)> {
        self.next_cell
    }

    #[cfg(test)]
    /// Observe the actual candidate mapping without retaining text. Tests cannot
    /// substitute a zero admission count for its final actual munmap.
    pub(super) fn replacement_backing_observer(
        &self,
    ) -> Option<crate::MarkdownOriginalBackingObserver> {
        self.replacement.backing_observer()
    }
}

#[derive(Debug)]
/// Keep draft-admission failure distinct from the original canonical transaction's
/// cause. Observer/journal dual failure must not collapse into ordinary Stale.
pub enum MarkdownCellDraftCommitError<E> {
    Draft(MarkdownCellDraftError),
    Edit(MarkdownObservedEditError<E>),
}

impl<E> From<MarkdownCellDraftError> for MarkdownCellDraftCommitError<E> {
    fn from(error: MarkdownCellDraftError) -> Self {
        Self::Draft(error)
    }
}

impl<E: std::fmt::Display> std::fmt::Display for MarkdownCellDraftCommitError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Draft(error) => error.fmt(f),
            Self::Edit(error) => error.fmt(f),
        }
    }
}
impl<E: std::error::Error + 'static> std::error::Error for MarkdownCellDraftCommitError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Draft(error) => Some(error),
            Self::Edit(error) => Some(error),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Original word group and first-hit position from the same draft input. Map
/// coordinates only, without querying/expanding the group again. A Shift anchor
/// uses an empty range at that same first hit; its gesture owner retains the role.
pub struct MarkdownCellDraftWordAnchor {
    /// Actual original group ends. Trimming may genuinely collapse the group to
    /// empty; that result cannot select adjacent content instead.
    pub range: MarkdownTextSelection,
    /// First-hit position used for original within-word drag direction. Neither a
    /// group midpoint nor a later pointer location can replace it.
    pub origin_utf16: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Actual coordinates produced by this document commit, not successor Source
/// authority or responsibility for moving canonical selection. The caller checks
/// generation together with the original Body result before applying them to text.
pub struct MarkdownCellDraftCommitReceipt {
    /// Original directed draft anchor/head mapped into this canonical text's
    /// UTF-16 coordinates by the same sanitization pass.
    pub selection: MarkdownTextSelection,
    /// Original Word group mapped into canonical text in that same pass. A Shift
    /// anchor has equal ends. Trimming may leave a real empty group, never a new
    /// neighboring word selection; None means this commit had no Word gesture.
    pub word_anchor: Option<MarkdownCellDraftWordAnchor>,
    /// Only actual final-cell append supplies next-cell query coordinates.
    /// Activation still requires the successor generation's real row/cell Source.
    pub next_cell: Option<(usize, usize)>,
}

#[derive(Debug)]
/// Preserve single-transaction and unchanged-content semantics while returning the
/// actual mapped endpoints, avoiding a whole-cell reread after draft closure.
pub struct MarkdownCellDraftCommit<O> {
    /// None runs no observer, moves no canonical selection and creates no empty
    /// Undo; the receipt still belongs to this same successfully closed draft.
    pub edit: Option<(MarkdownHistoryTextEdit, O)>,
    pub receipt: MarkdownCellDraftCommitReceipt,
}

/// Failure retains the original draft. Return a result/endpoint receipt only after
/// actual publication or successful unchanged-content closure.
pub type MarkdownCellDraftCommitResult<O> = Result<
    MarkdownCellDraftCommit<<O as MarkdownTextChangeObserver>::Output>,
    MarkdownCellDraftCommitError<<O as MarkdownTextChangeObserver>::Error>,
>;

impl PendingCellDraft {
    fn replacement(
        &self,
        document: &MarkdownDocumentBuffer,
        word_anchor: Option<MarkdownCellDraftWordAnchor>,
    ) -> Result<
        (
            CellReplacementOutput,
            MarkdownTextSelection,
            Option<MarkdownCellDraftWordAnchor>,
        ),
        MarkdownCellDraftError,
    > {
        self.require(document)?;
        let (_, prefix) = self.plan.replacement_for(document)?;
        let source = DraftSource {
            document,
            edits: &self.edits,
            scope: &self.scope,
            serial: self.serial,
        };
        let metrics = piece_tree_metrics(&self.root);
        if word_anchor.is_some_and(|anchor| {
            anchor.range.anchor_utf16 > metrics.utf16_len
                || anchor.range.head_utf16 > metrics.utf16_len
                || anchor.origin_utf16 > metrics.utf16_len
        }) {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument.into());
        } // Admit only actual child endpoints before writing; invalid bounds cannot clamp into another word or Source.
        let mut sanitized = CellReplacementOutput::new(metrics, prefix)?;
        // Keep only input selection ends, first-word ends and first-hit position.
        // No per-character map or second long-cell pass is needed. Missing-cell
        // prefix is already written, so local zero follows it with direction intact.
        let mut selection = MarkdownTextSelection::caret(sanitized.utf16_len());
        let mut mapped_word_anchor = word_anchor.map(|_| MarkdownCellDraftWordAnchor {
            range: selection,
            origin_utf16: sanitized.utf16_len(),
        });
        let mut input_utf16 = 0;
        let mut odd_backslashes = false;
        let mut last_is_space = false;
        let mut started = false;
        // Prefix is outside text trimming. Keep only the last non-whitespace
        // metrics; long trailing whitespace may stream to disk and then truncate.
        let mut trimmed_end = (sanitized.len(), sanitized.utf16_len());
        let result = visit_piece_tree_range(
            &source,
            self.root.as_deref(),
            0..piece_tree_metrics(&self.root).byte_len,
            0,
            &mut |bytes| {
                let text = std::str::from_utf8(bytes)
                    .map_err(|_| MarkdownDocumentBufferError::InvalidUtf8Document)?;
                for scalar in text.chars() {
                    // Validate original group ends during the same scalar pass.
                    // Surrogate interiors are invalid; reject only this candidate,
                    // leaving original draft, selection and input authority intact.
                    let next_utf16 = input_utf16 + scalar.len_utf16();
                    if word_anchor.is_some_and(|anchor| {
                        [
                            anchor.range.anchor_utf16,
                            anchor.range.head_utf16,
                            anchor.origin_utf16,
                        ]
                        .into_iter()
                        .any(|point| input_utf16 < point && point < next_utf16)
                    }) {
                        return Err(MarkdownDocumentBufferError::NotUnicodeBoundary);
                    }
                    // Fold CR/LF to spaces without duplicate separators. Backslash
                    // parity continues across actual 4 KiB logical leaf boundaries.
                    let needs_space = !last_is_space;
                    let mut emit = |value: char| -> Result<(), MarkdownDocumentBufferError> {
                        last_is_space = value == ' '; // Preserve the rule's ends-with-space state even when global leading whitespace is skipped.
                        if !started && value.is_whitespace() {
                            return Ok(()); // Skip only the global prefix, never whitespace at every internal piece boundary.
                        }
                        started = true;
                        let mut encoded = [0; 4];
                        sanitized.push_text(value.encode_utf8(&mut encoded))?;
                        if !value.is_whitespace() {
                            trimmed_end = (sanitized.len(), sanitized.utf16_len());
                            if trimmed_end.0 > document.maximum_document_bytes {
                                // Confirmed retained text exceeds the real document
                                // limit. Temporary trailing whitespace does not count as final payload.
                                return Err(MarkdownDocumentBufferError::DocumentTooLarge);
                            }
                        }
                        Ok(())
                    };
                    match scalar {
                        '\r' | '\n' => {
                            if needs_space {
                                emit(' ')?;
                            }
                            odd_backslashes = false;
                        }
                        '|' if !odd_backslashes => {
                            emit('\\')?;
                            emit('|')?;
                            odd_backslashes = false;
                        }
                        '\\' => {
                            emit('\\')?;
                            odd_backslashes = !odd_backslashes;
                        }
                        _ => {
                            emit(scalar)?;
                            odd_backslashes = false;
                        }
                    }
                    // Map positions only after a scalar's complete escape/folding.
                    // CRLF interior and folded-LF end may coincide. Original input
                    // boundaries were verified, so inserted escape bytes or
                    // surrogate interiors cannot impersonate its original caret.
                    input_utf16 = next_utf16;
                    if input_utf16 == self.selection.anchor_utf16 {
                        selection.anchor_utf16 = sanitized.utf16_len();
                    }
                    if input_utf16 == self.selection.head_utf16 {
                        selection.head_utf16 = sanitized.utf16_len();
                    }
                    if let Some(original) = word_anchor {
                        let mapped = mapped_word_anchor.as_mut().expect("原首组有同趟映射槽");
                        if original.range.anchor_utf16 == input_utf16 {
                            mapped.range.anchor_utf16 = sanitized.utf16_len();
                        }
                        if original.range.head_utf16 == input_utf16 {
                            mapped.range.head_utf16 = sanitized.utf16_len();
                        }
                        if original.origin_utf16 == input_utf16 {
                            mapped.origin_utf16 = sanitized.utf16_len();
                        }
                    }
                }
                Ok(())
            },
        );
        if let Err(error) = result {
            self.require(document)?; // Original Source failure precedes output-capacity or candidate-file failure.
            return Err(match error {
                MarkdownDocumentBufferError::DocumentTooLarge => {
                    MarkdownCellDraftError::CommitLimit
                }
                other => other.into(),
            });
        }
        // Apply the existing Unicode trim once. After actual anonymous-file
        // truncation/repositioning, final-cell append can continue the same output.
        sanitized.truncate(trimmed_end.0, trimmed_end.1)?;
        // Clamp each directed endpoint to retained text after true tail trimming,
        // without swapping reverse selection. Check absolute overflow during
        // preparation; publication adds no fallible coordinate work or Source read.
        selection.anchor_utf16 = self
            .range
            .start
            .checked_add(selection.anchor_utf16.min(trimmed_end.1))
            .ok_or(MarkdownCellDraftError::CommitLimit)?;
        selection.head_utf16 = self
            .range
            .start
            .checked_add(selection.head_utf16.min(trimmed_end.1))
            .ok_or(MarkdownCellDraftError::CommitLimit)?;
        let word_anchor = mapped_word_anchor
            .map(|anchor| {
                Ok::<_, MarkdownCellDraftError>(MarkdownCellDraftWordAnchor {
                    range: MarkdownTextSelection {
                        anchor_utf16: self
                            .range
                            .start
                            .checked_add(anchor.range.anchor_utf16.min(trimmed_end.1))
                            .ok_or(MarkdownCellDraftError::CommitLimit)?,
                        head_utf16: self
                            .range
                            .start
                            .checked_add(anchor.range.head_utf16.min(trimmed_end.1))
                            .ok_or(MarkdownCellDraftError::CommitLimit)?,
                    },
                    origin_utf16: self
                        .range
                        .start
                        .checked_add(anchor.origin_utf16.min(trimmed_end.1))
                        .ok_or(MarkdownCellDraftError::CommitLimit)?,
                })
            })
            .transpose()?; // A whitespace group may collapse to empty; preserve it rather than selecting adjacent content.
        self.require(document)?;
        Ok((sanitized, selection, word_anchor))
    }

    fn unchanged(
        &self,
        document: &MarkdownDocumentBuffer,
        replacement: &CellReplacement,
    ) -> Result<bool, MarkdownCellDraftError> {
        let start = document.byte_offset_for_utf16(self.range.start)?;
        let end = document.byte_offset_for_utf16(self.range.end)?;
        if end - start != replacement.len() {
            return Ok(false);
        }
        let mut cursor = 0;
        let mut same = true;
        visit_piece_tree_range(
            document,
            document.root.as_deref(),
            start..end,
            0,
            &mut |bytes| {
                // Finish reading the original range: finding different text early
                // cannot conceal a later actual I/O failure.
                same &= replacement.matches_bytes(cursor, bytes)?;
                cursor += bytes.len();
                Ok(())
            },
        )?;
        self.require(document)?;
        Ok(same && cursor == replacement.len())
    }
}

impl MarkdownDocumentBuffer {
    /// Consume original input authority to prepare bounded/streamed sanitization,
    /// allowing two actual Scene readers to keep the original content. End marked
    /// input through its original IME protocol first. Failure changes neither draft,
    /// selection, history, reader slots nor canonical text.
    pub fn prepare_cell_draft_commit(
        &self,
        edit: super::edit::MarkdownCellDraftEditCapsule,
        word_anchor: Option<MarkdownCellDraftWordAnchor>,
    ) -> Result<MarkdownCellDraftPreparedCommit, MarkdownCellDraftError> {
        let pending = self.require_cell_draft_edit(&edit)?;
        if pending.marked_range.is_some() {
            return Err(MarkdownCellDraftError::CompositionActive);
        }
        self.prepare_cell_draft_commit_inner(pending, word_anchor)
    }

    fn prepare_cell_draft_commit_inner(
        &self,
        pending: &PendingCellDraft,
        word_anchor: Option<MarkdownCellDraftWordAnchor>,
    ) -> Result<MarkdownCellDraftPreparedCommit, MarkdownCellDraftError> {
        // Reserve before any String allocation. On failure, actual local output
        // storage drops before the claim reopens the unique admission slot.
        let mut claim = pending.scope.reserve_commit()?;
        let candidate =
            pending
                .replacement(self, word_anchor)
                .and_then(|(output, selection, word_anchor)| {
                    let (replacement, units) = output.seal(&mut claim)?;
                    Ok((replacement, units, selection, word_anchor))
                });
        let (replacement, replacement_utf16, selection, word_anchor) = match candidate {
            Ok(value) => value,
            Err(error) => {
                pending.require(self)?; // Anonymous-candidate I/O failure cannot conceal the original Source's first cause.
                return Err(error);
            }
        };
        claim.allocated(replacement.capacity());
        let unchanged = pending.unchanged(self, &replacement)?;
        pending.require(self)?;
        Ok(MarkdownCellDraftPreparedCommit {
            scope: Arc::downgrade(&pending.scope),
            serial: pending.serial,
            range: pending.range.clone(),
            replacement,
            replacement_utf16,
            selection,
            word_anchor,
            unchanged,
            next_cell: None,
            _claim: claim,
        })
    }

    /// Combine original final-cell input and actual table-tail append in the same
    /// admission receipt. Existing Scene readers may survive bounded preparation.
    /// Marked state, input serial, same-document Source and verified End must all
    /// permit the operation; failure preserves the original raw draft.
    pub fn prepare_cell_draft_row_append(
        &self,
        edit: super::edit::MarkdownCellDraftEditCapsule,
        terminal: &VerifiedMarkdownTableRowSource,
        authority: MarkdownViewportSourceAuthority,
        word_anchor: Option<MarkdownCellDraftWordAnchor>,
    ) -> Result<MarkdownCellDraftPreparedCommit, MarkdownCellDraftError> {
        let pending = self.require_cell_draft_edit(&edit)?;
        if pending.marked_range.is_some() {
            return Err(MarkdownCellDraftError::CompositionActive);
        }
        let prepared = (|| {
            let end = terminal
                .terminal_end_for(self, authority)
                .map_err(|_| MarkdownCellDraftError::Stale)?
                .ok_or(MarkdownTableWindowError::ContextPending)?;
            let (header, row, columns) = terminal
                .coordinate_for(self, authority)
                .map_err(|_| MarkdownCellDraftError::Stale)?;
            let context = &pending.plan.cell.context;
            if header != context.header.start
                || row != context.row_ordinal
                || columns != context.columns
            {
                // Another actual row/table's End cannot serve this draft. Equal
                // range length or insertion anchor is insufficient evidence.
                return Err(MarkdownTableWindowError::InvalidSource.into());
            }
            // Reserve the unique slot before any missing-cell/sanitization String
            // allocation. Busy creates no additional candidate copy.
            let claim = pending.scope.reserve_commit()?;
            // Activation's original receipt supplies raw endpoints/separators.
            // Attach only this actual End, without rereading a giant cell or
            // constructing a complete table window again.
            let mut cell = pending.plan.cell.clone();
            cell.context.end = Some(end.clone());
            let target = self.plan_table_cell_source_replacement(cell, "")?;
            self.prepare_cell_draft_append_inner(pending, &target, claim, word_anchor)
        })();
        if prepared.is_err() {
            // Recheck original health even after the adapter returns a Table
            // error. Actual reclaim/read failure cannot become invalid coordinates.
            pending.require(self)?;
        }
        prepared
    }

    /// A long cell may already have same-source End at activation. Final-cell
    /// append consumes that original plan without requiring whole-row preview Ready.
    pub fn prepare_cell_draft_original_row_append(
        &self,
        edit: super::edit::MarkdownCellDraftEditCapsule,
        word_anchor: Option<MarkdownCellDraftWordAnchor>,
    ) -> Result<MarkdownCellDraftPreparedCommit, MarkdownCellDraftError> {
        let pending = self.require_cell_draft_edit(&edit)?;
        if pending.marked_range.is_some() {
            return Err(MarkdownCellDraftError::CompositionActive);
        }
        let claim = pending.scope.reserve_commit()?;
        let prepared =
            self.prepare_cell_draft_append_inner(pending, &pending.plan, claim, word_anchor);
        if prepared.is_err() {
            pending.require(self)?;
        }
        prepared
    }

    /// These two entries differ only in End's origin: a successor terminal or the
    /// original cell plan. Sanitization, tail output and unique publication are shared.
    fn prepare_cell_draft_append_inner(
        &self,
        pending: &PendingCellDraft,
        target: &MarkdownTableCellEditPlan,
        mut claim: CommitClaim,
        word_anchor: Option<MarkdownCellDraftWordAnchor>,
    ) -> Result<MarkdownCellDraftPreparedCommit, MarkdownCellDraftError> {
        let (target_range, target_prefix) = target.replacement_for(self)?;
        let (original_range, original_prefix) = pending.plan.replacement_for(self)?;
        if target_range != original_range || target_prefix != original_prefix {
            return Err(MarkdownTableWindowError::InvalidSource.into());
        }
        // Keep the same-rule sanitized writer unsealed. Actual tail/new-row bytes
        // continue directly, avoiding a second long-cell copy for Tab.
        let (mut replacement, selection, word_anchor) = pending.replacement(self, word_anchor)?;
        let tail = self.verified_cell_row_append_tail_for_plan(target)?;
        let next_cell = tail.next_cell();
        let range = tail.replacement_range_for(self)?.clone();
        if replacement
            .len()
            .checked_add(tail.byte_len())
            .is_none_or(|bytes| bytes > self.maximum_document_bytes)
        {
            return Err(MarkdownDocumentBufferError::DocumentTooLarge.into()); // Reject known oversized live output before writing a giant tail.
        }
        replacement.prepare_suffix(tail.byte_len())?;
        tail.write_to(self, &mut |bytes| replacement.push_bytes(bytes))?;
        let (replacement, replacement_utf16) = replacement.seal(&mut claim)?;
        claim.allocated(replacement.capacity());
        pending.require(self)?;
        Ok(MarkdownCellDraftPreparedCommit {
            scope: Arc::downgrade(&pending.scope),
            serial: pending.serial,
            range,
            replacement,
            replacement_utf16,
            selection, // Original draft endpoints were sealed before tail append; new-row bytes cannot move them into the next cell.
            word_anchor,
            unchanged: false,
            next_cell: Some(next_cell),
            _claim: claim,
        })
    }

    /// Consume the same original admission only after both actual Scene readers
    /// have returned, performing one canonical transaction. Do not sanitize again
    /// or recapture successor input. Foreign/stale serials, Source failure and busy
    /// readers all retain the original draft.
    pub fn commit_prepared_cell_draft_observed<O: MarkdownTextChangeObserver>(
        &mut self,
        mut prepared: MarkdownCellDraftPreparedCommit,
        observer: O,
    ) -> MarkdownCellDraftCommitResult<O> {
        let pending = self.active_cell_draft_scope(&prepared.scope)?;
        if pending.serial != prepared.serial {
            return Err(MarkdownCellDraftError::Stale.into());
        }
        if pending.scope.has_readers() {
            return Err(MarkdownCellDraftError::ReadersBusy.into());
        }
        if let Err(error) = prepared.replacement.health() {
            pending.require(self)?; // Original Source failure comes first; candidate corruption cannot cancel the still-editable raw branch.
            return Err(MarkdownCellDraftError::from(error).into());
        }
        let Some(CellDraftSlot::Active(pending)) = self.cell_draft.take() else {
            unreachable!("已核唯一活动草稿");
        };
        if prepared.unchanged {
            // Close an unchanged draft without observers, an empty change journal,
            // or an empty Undo entry, preserving the field's existing behavior.
            pending.scope.cancelled.store(true, Ordering::Release);
            drop(pending);
            return Ok(MarkdownCellDraftCommit {
                edit: None,
                receipt: MarkdownCellDraftCommitReceipt {
                    selection: prepared.selection,
                    word_anchor: prepared.word_anchor,
                    next_cell: prepared.next_cell,
                },
            });
        }
        let result = (|| {
            let after = prepared
                .range
                .start
                .checked_add(prepared.replacement_utf16)
                .ok_or_else(|| {
                    MarkdownCellDraftCommitError::Draft(
                        MarkdownDocumentBufferError::DocumentTooLarge.into(),
                    )
                })?;
            let input = self.prepare_text_input_replacement_observed(
                prepared.range.clone(),
                prepared.replacement.input(prepared.replacement_utf16),
                self.selection,
                MarkdownTextSelection::caret(after),
                observer,
            );
            let (input, output) = match input {
                Ok(value) => value,
                Err(error) => {
                    pending.require(self)?; // Preserve the original Source first cause under Draft after transaction preparation fails.
                    return Err(MarkdownCellDraftCommitError::Edit(error));
                }
            };
            // An observer may expose a sticky failure in shared Source. Revalidate
            // the original draft before the canonical publication boundary.
            if let Err(error) = pending.require(self) {
                self.cancel_prepared_text_input(&input)
                    .map_err(|error| MarkdownCellDraftCommitError::Draft(error.into()))?;
                return Err(error.into());
            }
            match self.publish_text_input(&input) {
                Ok(edit) => {
                    if matches!(&prepared.replacement, CellReplacement::Frozen(_)) {
                        // This same actual file is now a published segment. The
                        // document's backing traversal owns history/final release.
                        // Inline's temporary mapping already released at seal and
                        // cannot pretend to transfer file ownership here.
                        prepared._claim.release_backing_ticket();
                    }
                    Ok((edit, output))
                }
                Err(error) => {
                    // The actual candidate has not published. Original cancel
                    // drops its root before rolling back canonical appends/journal.
                    self.cancel_prepared_text_input(&input)
                        .map_err(|cleanup| MarkdownCellDraftCommitError::Draft(cleanup.into()))?;
                    Err(MarkdownCellDraftCommitError::Draft(error.into()))
                }
            }
        })();
        match result {
            Ok(value) => {
                pending.scope.cancelled.store(true, Ordering::Release);
                // Canonical text retains only final replacement storage. Drop the
                // actual raw branch instead of transferring unused draft delta
                // segments into the canonical session inventory.
                drop(pending);
                Ok(MarkdownCellDraftCommit {
                    edit: Some(value),
                    receipt: MarkdownCellDraftCommitReceipt {
                        selection: prepared.selection,
                        word_anchor: prepared.word_anchor,
                        next_cell: prepared.next_cell,
                    },
                })
            }
            Err(error) => {
                self.cell_draft = Some(CellDraftSlot::Active(pending));
                Err(error)
            }
        }
    }
}
