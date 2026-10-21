//! Source editing consumes Core keyboard commands; Native does not duplicate marker continuation, indentation, or history.

use super::*;
use omni_markdown_core::{MarkdownEditingCommand, MarkdownEditingOutcome};

impl NativeMarkdownPagedInput {
    /// Insert the existing Chinese table template and first-cell caret in one Body transaction without copying replaced text.
    ///
    /// # Errors
    /// Composition, multiple selection, and Cell cannot insert Body blocks; permit/Source/preparation/publication failures preserve text/history.
    pub fn insert_default_table(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputCommit> {
        let _mutation = self.begin(permit, current)?;
        if self.is_cell() {
            return Err(NativeMarkdownPagedInputError::BodyTargetRequired);
        } // A draft cannot turn local offsets into canonical block coordinates through Insert Table.
        if permit.before.marked.is_some() {
            return Err(NativeMarkdownPagedInputError::CompositionActive);
        } // The system IME owns its candidate; the button cannot force confirmation/cancellation and recapture another selection.
        if !permit.before.selection_is_directed {
            return Err(NativeMarkdownPagedInputError::MultipleSelectionInput);
        }
        let range = permit.before.primary_range;
        let end = range
            .location
            .checked_add(range.length)
            .ok_or(NativeMarkdownPagedInputError::InvalidRange)?;
        let original = self
            .document_identity()
            .upgrade()
            .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
        let observer = self.observer(permit.before)?;
        let (token, prepared) = {
            let mut document = original
                .try_borrow_mut()
                .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
            // Read only adjacent UTF-16 units at selection endpoints; propagate actual I/O failure rather than treat it as a non-newline.
            let needs_prefix = range.location > 0
                && !matches!(document.utf16_code_unit_at(range.location - 1)?, 10 | 13);
            let needs_suffix =
                end < document.utf16_len() && !matches!(document.utf16_code_unit_at(end)?, 10 | 13);
            let markdown =
                super::super::native_markdown_default_table_source(needs_prefix, needs_suffix);
            let first = range
                .location
                .checked_add(usize::from(needs_prefix) * 2 + 2)
                .ok_or(NativeMarkdownPagedInputError::InvalidRange)?;
            document.prepare_text_input_observed(
                range.location..end,
                &markdown,
                permit.before.selection,
                MarkdownTextSelection::caret(first),
                observer,
            )?
        };
        // Preparation publishes no text. Release the document borrow before foreground reentry; revoke only this nonce on failure.
        if let Err(cause) = self.check_permit(permit, current) {
            return Err(self.cancel(&original, &token, cause));
        }
        let mut document = match original.try_borrow_mut() {
            Ok(document) => document,
            Err(_) => {
                return Err(self.cancel(
                    &original,
                    &token,
                    NativeMarkdownPagedInputError::Borrowed,
                ));
            }
        };
        let mut folds = match self.folds.try_borrow_mut() {
            Ok(folds) => folds,
            Err(_) => {
                drop(document);
                return Err(self.cancel(
                    &original,
                    &token,
                    NativeMarkdownPagedInputError::Borrowed,
                ));
            }
        };
        let edit = match document.publish_text_input(&token) {
            Ok(edit) => edit,
            Err(cause) => {
                drop(folds);
                drop(document);
                return Err(self.cancel(&original, &token, cause.into()));
            }
        };
        // After Core publication exchange prepared selection/fold metadata only, with no Native call or second caret transaction.
        let commit = self.publish(permit, &document, prepared, None, Some(edit), &mut folds);
        drop(folds);
        drop(document);
        drop(original);
        drop(token);
        Ok(commit)
    }

    /// Submit one Source command under the original permit; ordinary Backspace consumes only its caller's same-Scene verified grapheme range.
    ///
    /// None means Core has no applicable marker rule and no grapheme plan was supplied; the caller must retain its actual
    /// Pending/error rather than report deletion. Consume one permit without recapture or replay.
    ///
    /// # Errors
    /// Marked input, unknown multi-range direction, Cell coordinates, stale permit, or resource/Core preflight failure preserves text/history.
    pub fn apply_source_editing_command(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        command: MarkdownEditingCommand,
        ordinary_backward: Option<NSRange>,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<Option<NativeMarkdownPagedInputOutcome>> {
        let _mutation = self.begin(permit, current)?;
        if self.is_cell() {
            return Err(NativeMarkdownPagedInputError::BodyTargetRequired);
        } // The Cell controller owns table Tab/Return; local draft positions cannot drive canonical block commands.
        if permit.before.marked.is_some() {
            return Err(NativeMarkdownPagedInputError::CompositionActive);
        } // The IME still owns the candidate; block algorithms cannot remove its markers or end composition.
        if !permit.before.selection_is_directed {
            return Err(NativeMarkdownPagedInputError::MultipleSelectionInput);
        } // These commands require one directed selection; do not edit only a primary range and silently discard others.

        let original = self
            .document_identity()
            .upgrade()
            .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
        let observer = self.observer(permit.before)?;
        {
            // current finished outside borrows; Core/FoldObserver now perform pure Rust preflight and one atomic publication.
            let mut document = original
                .try_borrow_mut()
                .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
            let mut folds = self
                .folds
                .try_borrow_mut()
                .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
            if !self.matches_document(&document, permit.before) {
                return Err(NativeMarkdownPagedInputError::StateChanged);
            }
            let (outcome, prepared) = document.apply_editing_command_observed(
                command,
                permit.before.selection,
                observer,
            )?;
            match outcome {
                MarkdownEditingOutcome::Changed(edit) => {
                    // Core succeeded; exchange original fold/selection metadata without Native calls or fallible postcommit I/O.
                    return Ok(Some(NativeMarkdownPagedInputOutcome::Committed(
                        self.publish(permit, &document, prepared, None, Some(edit), &mut folds),
                    )));
                }
                MarkdownEditingOutcome::Unchanged => {
                    // BackTab on unindented text consumes the action without moving focus, advancing serial, or invalidating Redo.
                    return Ok(Some(NativeMarkdownPagedInputOutcome::Unchanged(
                        permit.before,
                    )));
                }
                MarkdownEditingOutcome::NotHandled => {
                    // No marker rule publishes no temporary metadata; release every borrow before checking the original grapheme current.
                }
            }
        }
        if let Some(range) = ordinary_backward {
            // Reuse replace/FoldObserver with the permit already consumed by begin and busy still held; do not acquire another permit.
            return self
                .replace(permit, "", range, None, None, current)
                .map(NativeMarkdownPagedInputOutcome::Committed)
                .map(Some);
        }
        Ok(None) // Without a verified grapheme range there is no ordinary deletion; View must preserve actual Pending/failure.
    }
}
