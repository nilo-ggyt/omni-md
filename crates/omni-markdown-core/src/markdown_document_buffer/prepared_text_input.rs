//! Completes fallible preparation before native input enters its superclass.
//! Publication consumes the same document's sole prepared-input authority.

use std::sync::Weak;
use std::sync::atomic::{AtomicBool, Ordering};

use super::changes::{MarkdownObservedEditError, MarkdownTextChangeObserver};
use super::command_transaction::{self, PreparedCommand, SealedCommand};
use super::*;

mod composition;
pub(super) use composition::CompositionGroup;
pub use composition::MarkdownTextInputComposition;

/// Two actual storage forms within the same transaction. Core alone owns frozen
/// replacement files; this does not add a public raw-range commit protocol.
pub(super) enum TextInputReplacement<'a> {
    Text(&'a str),
    Frozen {
        backing: Arc<OriginalBacking>,
        utf16: usize,
    },
}

impl TextInputReplacement<'_> {
    /// Both forms use their actual extent for the same resulting-document and
    /// session-append admission checks.
    fn byte_len(&self) -> usize {
        match self {
            Self::Text(text) => text.len(),
            Self::Frozen { backing, .. } => backing.len(),
        }
    }

    /// Frozen input carries the UTF-16 measure from its original scalar-cleaning
    /// pass, avoiding a second decode into a whole-file string.
    fn utf16_len(&self) -> usize {
        match self {
            Self::Text(text) => text.encode_utf16().count(),
            Self::Frozen { utf16, .. } => *utf16,
        }
    }
}

#[derive(Debug)]
struct InputNonce {
    cancelled: AtomicBool,
}

#[derive(Debug)]
/// The unique token retains no document, piece tree, mapping, or journal. Dropping
/// it only marks the original candidate cancelled, without document reentry.
///
/// Normal unborrowed exits cancel explicitly. After an exceptional token drop,
/// the owner must drain cancellation before claiming candidate resources released.
pub struct MarkdownPreparedTextInput {
    nonce: Weak<InputNonce>,
}

impl Drop for MarkdownPreparedTextInput {
    fn drop(&mut self) {
        if let Some(nonce) = self.nonce.upgrade() {
            // Mark only this nonce. A published or replaced token cannot revoke the
            // authority of a successor candidate.
            nonce.cancelled.store(true, Ordering::Release);
        }
    }
}

/// An empty replacement prepares selection only, without inventing a text-journal
/// entry, generation change, or history item.
enum PendingCommand {
    Characters(SealedCommand),
    SelectionOnly {
        after: MarkdownTextSelection,
        metrics: MarkdownReplacementMetrics,
    },
}

/// The document owns every resource; the token holds only a weak nonce. Separate
/// nonces isolate even two empty documents with otherwise identical roots.
pub(super) struct PendingTextInput {
    nonce: Arc<InputNonce>,
    original_root: PieceTreeLink,
    original_generation: u64,
    original_selection: MarkdownTextSelection,
    command: PendingCommand,
    composition: Option<composition::CompositionPlan>,
}

impl PendingTextInput {
    fn matches(&self, token: &MarkdownPreparedTextInput) -> bool {
        // Weak identity comparison neither retains text nor mistakes two empty
        // roots for the same document owner.
        Weak::ptr_eq(&token.nonce, &Arc::downgrade(&self.nonce))
    }

    fn discard(self, document: &mut MarkdownDocumentBuffer) {
        if let PendingCommand::Characters(prepared) = self.command {
            // The candidate may reference new append bytes. Drop its tree references
            // before restoring append boundaries and releasing the sealed journal.
            let SealedCommand {
                command,
                checkpoint,
                changes,
                ..
            } = prepared;
            drop(command);
            checkpoint.rollback(document);
            document.change_journal.release(changes);
        }
        // Composition copy-on-write storage has not been installed. Dropping the
        // plan releases it; the original checkpoint rolls back actual appends only.
        // A selection-only candidate owns no text or journal; the nonce ends with
        // this unique pending slot.
    }
}

impl MarkdownDocumentBuffer {
    /// Reads still see the old canonical root. Writes reject an unconsumed candidate;
    /// a cancellation flag alone never proves its resources have been released.
    pub(crate) fn ensure_text_input_idle(&self) -> Result<(), MarkdownDocumentBufferError> {
        self.original_backing_health()?;
        #[cfg(feature = "s2-table-window-prototype")]
        if self.cell_draft.is_some() {
            // A draft has not published text but still exclusively owns the original
            // edit baseline. A live cancelled read root cannot free that slot early.
            return Err(MarkdownDocumentBufferError::TextInputPrepared);
        }
        if self.prepared_text_input.is_some() {
            return Err(MarkdownDocumentBufferError::TextInputPrepared);
        }
        Ok(())
    }

    /// Reports actual candidate ownership, including a dropped token whose cancelled
    /// candidate has not yet been explicitly drained.
    #[must_use]
    pub fn has_prepared_text_input(&self) -> bool {
        self.prepared_text_input.is_some()
    }

    /// Drains the original drop-cancelled candidate without observers, new appends,
    /// or fallible journal I/O.
    ///
    /// Returns true only when this call releases a cancelled candidate. A candidate
    /// with a still-live token remains unchanged.
    pub fn drain_cancelled_text_input(&mut self) -> bool {
        if !self
            .prepared_text_input
            .as_ref()
            .is_some_and(|pending| pending.nonce.cancelled.load(Ordering::Acquire))
        {
            // Missing candidates or live tokens are not evidence of cancellation.
            // Leave legitimate prepared input intact.
            return false;
        }
        let pending = self
            .prepared_text_input
            .take()
            .expect("the cancellation check observed the unique pending slot");
        pending.discard(self);
        true
    }

    /// Cancels only the matching nonce. Foreign or stale tokens cannot truncate
    /// another candidate's appends or release its journal.
    ///
    /// # Errors
    /// A missing matching candidate returns stale authority without changing any
    /// document state.
    pub fn cancel_prepared_text_input(
        &mut self,
        token: &MarkdownPreparedTextInput,
    ) -> Result<(), MarkdownDocumentBufferError> {
        if !self
            .prepared_text_input
            .as_ref()
            .is_some_and(|pending| pending.matches(token))
        {
            // Verify before taking ownership. A wrong token cannot incidentally drain
            // the original candidate or a successor.
            return Err(MarkdownDocumentBufferError::PreparedTextInputStale);
        }
        let pending = self
            .prepared_text_input
            .take()
            .expect("the identity check observed the unique pending slot");
        pending.discard(self);
        Ok(())
    }

    /// Validates Unicode, actual size, append admission, derived output, and the real
    /// change journal before returning an invisible candidate.
    ///
    /// The caller stores the token together with its output. Output does not enter
    /// the document; the unique pending slot alone owns candidate text.
    /// All external callbacks must remain pure Rust. Between return and publication,
    /// canonical root, generation, selection, and history remain unchanged;
    /// ordinary reads cannot observe the candidate. Never validate it by temporarily
    /// replacing the canonical root.
    ///
    /// # Errors
    /// Range, size, observer, or journal failure uses shared transaction rollback.
    /// If journal cleanup also fails, retain both typed causes.
    pub fn prepare_text_input_observed<O: MarkdownTextChangeObserver>(
        &mut self,
        range: Range<usize>,
        replacement: &str,
        selection_before: MarkdownTextSelection,
        selection_after: MarkdownTextSelection,
        observer: O,
    ) -> Result<(MarkdownPreparedTextInput, O::Output), MarkdownObservedEditError<O::Error>> {
        self.prepare_text_input_replacement_observed(
            range,
            TextInputReplacement::Text(replacement),
            selection_before,
            selection_after,
            observer,
        )
    }

    /// String input and frozen cell output share the unique pending slot, journal,
    /// candidate root, and publication; only storage reading differs.
    pub(super) fn prepare_text_input_replacement_observed<O: MarkdownTextChangeObserver>(
        &mut self,
        range: Range<usize>,
        replacement: TextInputReplacement<'_>,
        selection_before: MarkdownTextSelection,
        selection_after: MarkdownTextSelection,
        observer: O,
    ) -> Result<(MarkdownPreparedTextInput, O::Output), MarkdownObservedEditError<O::Error>> {
        self.prepare_text_input_replacement_composition_observed(
            range,
            replacement,
            selection_before,
            selection_after,
            None,
            observer,
        )
    }

    /// Plain strings, frozen text, and real compositions share length measurement
    /// and candidate preparation. The optional value carries this input's IME owner.
    fn prepare_text_input_replacement_composition_observed<O: MarkdownTextChangeObserver>(
        &mut self,
        range: Range<usize>,
        replacement: TextInputReplacement<'_>,
        selection_before: MarkdownTextSelection,
        selection_after: MarkdownTextSelection,
        composition: Option<MarkdownTextInputComposition>,
        observer: O,
    ) -> Result<(MarkdownPreparedTextInput, O::Output), MarkdownObservedEditError<O::Error>> {
        self.ensure_text_input_idle()?;
        if range.start > range.end || range.end > self.utf16_len() {
            // Reject reversed or out-of-range coordinates before arithmetic or tree
            // splitting can hide the invalid original range.
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument.into());
        }
        self.validate_selection(selection_before)?;
        let byte_start = self.byte_offset_for_utf16(range.start)?;
        let byte_end = self.byte_offset_for_utf16(range.end)?;
        let inserted = replacement.utf16_len();
        let metrics = MarkdownReplacementMetrics::new(range.start, range.len(), inserted)?;
        let result_units = self
            .utf16_len()
            .checked_sub(range.len())
            .and_then(|length| length.checked_add(inserted))
            .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?;
        self.validate_selection_for_result(selection_after, result_units)?;
        if let TextInputReplacement::Text(text) = &replacement {
            // Strings retain their original scalar-boundary checks. Frozen input
            // validates both endpoints against bounded pieces of the candidate tree.
            self.validate_post_edit_utf16_boundary(
                selection_after.anchor_utf16,
                &range,
                text,
                inserted,
            )?;
            self.validate_post_edit_utf16_boundary(
                selection_after.head_utf16,
                &range,
                text,
                inserted,
            )?;
        }
        let mut composition = composition
            .map(|composition| self.plan_composition::<O::Error>(&range, inserted, composition))
            .transpose()?; // Reuse the measured UTF-16 length for net changes; do not rescan composition input.

        let (command, output) = if range.is_empty() && replacement.byte_len() == 0 {
            // Even a true no-text-change request validates its explicit selection.
            // Preparation must not move the canonical selection early.
            let output = observer
                .finish(
                    changes::MarkdownPreparedText {
                        document: self,
                        root: &self.root,
                    },
                    selection_after,
                )
                .map_err(MarkdownObservedEditError::Observer)?;
            (
                PendingCommand::SelectionOnly {
                    after: selection_after,
                    metrics,
                },
                output,
            )
        } else {
            let result_bytes = self
                .byte_len()
                .checked_sub(byte_end - byte_start)
                .and_then(|length| length.checked_add(replacement.byte_len()))
                .filter(|length| *length <= self.maximum_document_bytes)
                .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?;
            // Reserve before entering the native superclass. Full history evicts an
            // older entry first, so existing capacity needs no further growth.
            if self.undo.len() < MAX_EDIT_HISTORY_ENTRIES {
                self.undo
                    .try_reserve(1)
                    .map_err(|_| MarkdownDocumentBufferError::EditHistoryUnavailable)?;
            }
            let (mut prepared, output) = command_transaction::prepare_command_observed(
                self,
                selection_before,
                observer,
                |document, observer| {
                    // Use the regular transaction's actual change stream. Observer rejection
                    // precedes any text append.
                    changes::emit(document, observer, range, inserted)?;
                    let inserted = match replacement {
                        TextInputReplacement::Text(text) => {
                            if let Some(plan) = &mut composition {
                                // Acquire the actual exclusive allocation receipt with candidate leaves.
                                // An external range or length grants no retirement authority.
                                plan.append(
                                    &mut document.edits,
                                    text,
                                    document.maximum_document_bytes,
                                )?
                            } else {
                                document
                                    .edits
                                    .append(text, document.maximum_document_bytes)?
                            }
                        }
                        TextInputReplacement::Frozen { backing, .. } => {
                            // Take ownership of this exact frozen inode. A failed 4-KiB leaf
                            // measurement still belongs to the original checkpoint rollback.
                            document
                                .edits
                                .append_frozen(backing, document.maximum_document_bytes)?
                        }
                    };
                    let inserted = build_balanced_piece_tree(inserted);
                    let (prefix, remainder) =
                        split_piece_tree(document, document.root.clone(), byte_start)?;
                    let (_, suffix) = split_piece_tree(document, remainder, byte_end - byte_start)?;
                    if let Some(plan) = &mut composition {
                        plan.prepare_rebased_root(&prefix, &suffix);
                    }
                    Ok(PreparedCommand {
                        root: join_piece_trees(prefix, join_piece_trees(inserted, suffix)),
                        after: selection_after,
                        metrics,
                    })
                },
            )?;
            if let Some(plan) = &mut composition {
                // Candidate reads are complete. Substitute an equally measured root,
                // but install its copy-on-write bytes only at publication.
                plan.finish_prepared(&mut prepared);
            }
            debug_assert_eq!(
                piece_tree_metrics(&prepared.command.root).byte_len,
                result_bytes
            );
            (PendingCommand::Characters(prepared), output)
        };

        // Occupy the unique slot only after all fallible work succeeds. The nonce
        // is independent of root identity, so empty documents cannot share authority.
        let nonce = Arc::new(InputNonce {
            cancelled: AtomicBool::new(false),
        });
        let token = MarkdownPreparedTextInput {
            nonce: Arc::downgrade(&nonce),
        };
        self.prepared_text_input = Some(PendingTextInput {
            nonce,
            original_root: self.root.clone(),
            original_generation: self.generation,
            original_selection: self.selection,
            command,
            composition,
        });
        Ok((token, output))
    }

    /// Publishes once after consuming original nonce and complete canonical authority.
    /// No native call or observer runs after validation.
    /// Previous composition payload retires only after publication. Actual cleanup
    /// errors stay on its backing receipt, never masquerading as an uncommitted edit.
    ///
    /// # Errors
    /// Foreign, consumed, cancelled, or canonically stale tokens are rejected while
    /// leaving the candidate available to its rightful owner.
    pub fn publish_text_input(
        &mut self,
        token: &MarkdownPreparedTextInput,
    ) -> Result<MarkdownHistoryTextEdit, MarkdownDocumentBufferError> {
        // A frozen reader may have found a real reclamation failure since prepare.
        // The atomic health gate preserves the original errno.
        // On failure, retain the unique candidate and original nonce: no new root
        // is issued and explicit cancellation still owns resource rollback.
        self.original_backing_health()?;
        let valid = self.prepared_text_input.as_ref().is_some_and(|pending| {
            pending.matches(token)
                && !pending.nonce.cancelled.load(Ordering::Acquire)
                && pending.original_generation == self.generation
                && pending.original_selection == self.selection
                && piece_tree_links_share_root(&pending.original_root, &self.root)
        });
        if !valid {
            // Do not take the wrong slot: a stale token must never roll back appends
            // already prepared by a successor.
            return Err(MarkdownDocumentBufferError::PreparedTextInputStale);
        }
        self.edits.pending_health()?; // Check real candidate storage before taking it; rejection preserves explicit cancellation.
        let pending = self
            .prepared_text_input
            .take()
            .expect("the authority check observed the unique pending slot");
        let edit = match pending.command {
            PendingCommand::Characters(prepared) => {
                // History capacity is reserved. Publication only moves the root, journal
                // receipt, and entries within the fixed history budget.
                if let Some(plan) = pending.composition {
                    composition::publish(self, prepared, plan)
                } else {
                    command_transaction::publish_command(self, prepared)
                }
            }
            PendingCommand::SelectionOnly { after, metrics } => {
                // Selection-only input changes no generation, preserves redo, and creates
                // no empty history entry.
                self.selection = after;
                composition::publish_selection(self, pending.composition, after);
                metrics.redo_text_edit(after)
            }
        };
        Ok(edit)
    }
}

#[cfg(test)]
mod tests;
