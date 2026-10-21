//! Retain Find All's actual bounded ranges, distinguishing an ordered primary range from unknown primary direction.

use super::*;
use omni_markdown_core::{MarkdownReplacement, MarkdownReplacementPart};

/// Use Find All's existing range budget: it bounds metadata, not document size or total matches.
pub const NATIVE_MARKDOWN_PAGED_SELECTION_RANGE_LIMIT: usize = 1000;

impl NativeMarkdownPagedInput {
    /// Cut/deletion of nonempty selections consumes every actual range in one Core transaction.
    ///
    /// Single selection uses insert_text, including marked input, without adding bulk-only composition rejection. An empty caret has no
    /// selected bytes and Cut/delete: skips it; the command layer separately supplies Backspace/DeleteForward grapheme plans.
    ///
    /// # Errors
    /// Invalid original permit or Unicode/resource errors preserve state; the returned commit separately reports
    /// postcommit revocation. This API does not promise a multi-selection Undo graph that AppKit itself did not restore.
    pub fn delete_selected_ranges(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputOutcome> {
        let mut ranges = self.selected_ranges(permit, current)?;
        if permit.before.selection_is_directed {
            if permit.before.primary_range.length == 0 {
                let _mutation = self.begin(permit, current)?;
                self.check_permit(permit, current)?;
                return Ok(NativeMarkdownPagedInputOutcome::Unchanged(permit.before));
            }
            // The complete explicit single range, including marked input, bypasses the bulk Find path that rejects composition.
            return self
                .insert_text(permit, "", permit.before.primary_range, current)
                .map(NativeMarkdownPagedInputOutcome::Committed);
        }
        ranges.retain(|range| !range.is_empty());
        if ranges.is_empty() {
            let _mutation = self.begin(permit, current)?;
            self.check_permit(permit, current)?;
            return Ok(NativeMarkdownPagedInputOutcome::Unchanged(permit.before));
        }
        let removed = ranges.iter().try_fold(0_usize, |sum, range| {
            sum.checked_add(range.len())
                .ok_or(NativeMarkdownPagedInputError::InvalidRange)
        })?;
        let last = ranges
            .last()
            .ok_or(NativeMarkdownPagedInputError::StateChanged)?;
        let caret = last
            .end
            .checked_sub(removed)
            .ok_or(NativeMarkdownPagedInputError::InvalidRange)?;
        let replacements = ranges.into_iter().map(|range_utf16| {
            Ok(MarkdownReplacement {
                range_utf16,
                parts: std::iter::empty::<
                    std::result::Result<
                        MarkdownReplacementPart<'static>,
                        MarkdownDocumentBufferError,
                    >,
                >(),
            })
        });
        // The measured NSTextView multi-range deleteBackward places its caret at the last deletion's new end, not the first range.
        // Keep unselected gaps intact; Core before remains the permit's complete canonical memento.
        self.apply_replacements_observed(
            permit,
            replacements,
            MarkdownTextSelection::caret(caret),
            current,
        )
    }

    /// Snapshot the complete bounded range set under this permit without reading text or retaining a RefCell borrow across Native calls.
    ///
    /// # Errors
    /// Reject invalid original authority or conflicting mutation without recapturing multi-selection from a successor.
    pub fn selected_ranges(
        &self,
        permit: &NativeMarkdownPagedInputPermit,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<Vec<Range<usize>>> {
        self.check_permit(permit, current)?;
        let ranges = {
            let ranges = self
                .selection_ranges
                .try_borrow()
                .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
            if ranges.is_empty() {
                // Directed single selection uses the same range API; copy endpoint integers, not selected text.
                let range = permit.before.primary_range;
                std::iter::once(range.location..range.location + range.length).collect()
            } else {
                ranges.clone()
            }
        };
        self.check_permit(permit, current)?;
        Ok(ranges)
    }

    /// Accept Find's ordered, disjoint ranges; the primary range follows NSTextView's first-item rule with direction unknown.
    ///
    /// One explicit target authorizes anchor/head; multiple targets retain Core's history memento rather than substitute the first target,
    /// a whole-document range, or an envelope. The set has at most 1000 ranges and creates no text copy/history transaction.
    ///
    /// # Errors
    /// Active composition, bounds/scalar/order/budget errors, or lost original authority preserves the original state.
    pub fn set_selected_ranges(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        ranges: &[Range<usize>],
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputOutcome> {
        if let [range] = ranges {
            if range.start > range.end {
                // A single item is an ordered range, not reversed anchor/head; failure consumes the permit, forbidding payload replacement/retry.
                let _mutation = self.begin(permit, current)?;
                self.check_permit(permit, current)?;
                return Err(NativeMarkdownPagedInputError::InvalidRange);
            }
            // One explicit target establishes direction; the single-selection path consumes this same permit and checks scalar boundaries.
            return self.set_selection_intent(
                permit,
                MarkdownTextSelection {
                    anchor_utf16: range.start,
                    head_utf16: range.end,
                },
                None,
                None,
                current,
            );
        }
        let _mutation = self.begin(permit, current)?;
        self.check_permit(permit, current)?;
        if self.is_cell() {
            return Err(NativeMarkdownPagedInputError::BodyTargetRequired);
        } // Single Cell selection already used its local path; multiple targets cannot borrow Body's memento to invent local direction.
        if permit.before.marked.is_some() {
            return Err(NativeMarkdownPagedInputError::CompositionActive);
        }
        if ranges.is_empty() {
            return Ok(NativeMarkdownPagedInputOutcome::Unchanged(permit.before));
        }
        if ranges.len() > NATIVE_MARKDOWN_PAGED_SELECTION_RANGE_LIMIT {
            return Err(NativeMarkdownPagedInputError::InvalidSelectionRanges);
        }
        let original = self
            .document_identity()
            .upgrade()
            .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
        let document = original
            .try_borrow()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        if !self.matches_document(&document, permit.before) || document.has_prepared_text_input() {
            return Err(NativeMarkdownPagedInputError::StateChanged);
        }
        for (index, range) in ranges.iter().enumerate() {
            if range.start > range.end || range.end > permit.before.utf16_len {
                return Err(NativeMarkdownPagedInputError::InvalidRange);
            }
            if index > 0 {
                let previous = &ranges[index - 1];
                if range.start < previous.end
                    || (range.is_empty() && previous.is_empty() && range.start == previous.start)
                {
                    // Allow adjacent distinct matches; reject overlap, reversed order, and duplicate carets without sorting away malformed input.
                    return Err(NativeMarkdownPagedInputError::InvalidSelectionRanges);
                }
            }
            document.byte_offset_for_utf16(range.start)?;
            document.byte_offset_for_utf16(range.end)?;
        }
        let mut stored = self
            .selection_ranges
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        let previous = std::mem::replace(&mut *stored, ranges.to_vec());
        drop(previous); // Replace bounded range metadata and release old capacity; retain no document payload.
        let serial = self.selection.publish(
            permit.before.generation,
            permit.before.selection,
            true,
            None,
            None,
        );
        let primary = &ranges[0];
        let state = NativeMarkdownPagedInputState {
            selection_is_directed: false,
            selection_range_count: ranges.len(),
            primary_range: NSRange::new(primary.start, primary.len()),
            selection_serial: serial,
            ..permit.before
        };
        drop(stored);
        drop(document);
        drop(original);
        Ok(NativeMarkdownPagedInputOutcome::Committed(
            NativeMarkdownPagedInputCommit {
                owner: Rc::downgrade(self),
                epoch: permit.epoch,
                state,
                invalidation: NativeMarkdownPagedInputInvalidation {
                    replaced_utf16_range: None,
                    change_in_utf16_length: 0,
                    characters_changed: false,
                    selection_changed: true,
                    marked_changed: false,
                    folds_changed: false,
                },
            },
        ))
    }
}
