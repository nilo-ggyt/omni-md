//! Finite selection authority during the original character-commit notification.
//! Rejection remains irreversible for that same owner's entire lifetime.

use super::*;

#[derive(Clone, Copy, Debug)]
pub(in crate::native_surface) enum NativeMarkdownPendingSelectionPhase {
    Published,
    EditedNotification,
    Attributes,
    EndEditing,
    SuperclassTail,
}

/// Coordinates come only from original input or a pure Core edit result.
/// Observed Native values can never extend this candidate-source set.
#[derive(Clone, Copy)]
pub(in crate::native_surface) struct NativeMarkdownPendingSelectionSources {
    pub(in crate::native_surface) source: &'static str,
    pub(in crate::native_surface) replacement: Option<NSRange>,
    pub(in crate::native_surface) caret: Option<NSRange>,
    pub(in crate::native_surface) marked_before: Option<NSRange>,
    pub(in crate::native_surface) marked_after: Option<NSRange>,
    // Only the character primitive matching original prepared input may declare
    // the superclass deletion phase's transient EOF.
    pub(in crate::native_surface) is_input_replacement: bool,
}

impl NativeMarkdownPendingSelectionSources {
    /// Derive finite replacement/caret facts from the original canonical change,
    /// without granting the distinct Native input-replacement phase permission.
    pub(in crate::native_surface) fn canonical(
        source: &'static str,
        edit: &MarkdownHistoryTextEdit,
    ) -> Self {
        let inserted = edit
            .replaced_utf16_range
            .len()
            .checked_add_signed(edit.change_in_utf16_length);
        let caret = inserted
            .and_then(|length| edit.replaced_utf16_range.start.checked_add(length))
            .map(|offset| NSRange::new(offset, 0));
        Self {
            source,
            replacement: Some(NSRange::new(
                edit.replaced_utf16_range.start,
                edit.replaced_utf16_range.len(),
            )),
            caret,
            marked_before: None,
            marked_after: None,
            is_input_replacement: false,
        }
    }
}

/// Derive both additional endpoints only from original lengths/replacement.
/// A getter's observed value cannot fill in missing source facts.
fn native_markdown_pending_selection_eof_sources(
    before_len: usize,
    after_len: usize,
    sources: NativeMarkdownPendingSelectionSources,
) -> Option<(NSRange, Option<NSRange>)> {
    let replacement = sources.replacement?;
    let replacement_end = replacement.location.checked_add(replacement.length)?;
    if replacement_end > before_len {
        return None; // The replacement must belong wholly to the pre-Native root; out-of-range input grants no EOF authority.
    }
    let caret = sources.caret?;
    if caret.length != 0 {
        return None; // The insertion endpoint is a single original caret, not an arbitrary range standing in for input length.
    }
    let inserted_len = caret.location.checked_sub(replacement.location)?;
    let deleted_len = before_len.checked_sub(replacement.length)?;
    if deleted_len.checked_add(inserted_len)? != after_len {
        return None; // Original replacement and candidate length must agree exactly, without raw-value substitution or clamping.
    }
    Some((
        NSRange::new(before_len, 0),
        sources
            .is_input_replacement
            .then_some(NSRange::new(deleted_len, 0)),
    ))
}

/// The same actual receipt Rc crosses notifications, pool drains, and the
/// superclass tail, without retaining document text or a Native graph.
pub(in crate::native_surface) struct NativeMarkdownPendingSelectionOwner {
    pub(in crate::native_surface) id: u64,
    pub(in crate::native_surface) committed: Rc<NativeMarkdownCommittedSelection>,
    pub(in crate::native_surface) phase: Cell<NativeMarkdownPendingSelectionPhase>,
    pub(in crate::native_surface) source: &'static str,
    sampling: Cell<bool>,
    revoked: Cell<bool>,
    #[cfg(feature = "native-markdown-runtime-probe")]
    reads: Rc<Cell<u64>>,
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(in crate::native_surface) fault: Option<
        std::rc::Weak<super::super::probe::pending_fault::NativeMarkdownPendingSelectionFault>,
    >,
    ranges: [Option<NSRange>; 9],
}

pub(in crate::native_surface) type NativeMarkdownPendingSelectionSlot =
    RefCell<Option<Rc<NativeMarkdownPendingSelectionOwner>>>;

/// Take only the slot with original id and exact Rc identity. An old local guard
/// cannot remove a successor owner published through Native reentry.
fn take_pending_selection_owner(
    slot: &NativeMarkdownPendingSelectionSlot,
    owner: &Rc<NativeMarkdownPendingSelectionOwner>,
) -> Option<Rc<NativeMarkdownPendingSelectionOwner>> {
    let mut slot = slot.borrow_mut();
    if slot
        .as_ref()
        .is_some_and(|actual| actual.id == owner.id && Rc::ptr_eq(actual, owner))
    {
        slot.take()
    } else {
        None
    }
}

impl NativeMarkdownPendingSelectionOwner {
    /// The installation tail still requires this exact slot Rc; equal serials
    /// or another local owner of the same receipt cannot substitute for it.
    pub(in crate::native_surface) fn owns_slot(
        self: &Rc<Self>,
        slot: &NativeMarkdownPendingSelectionSlot,
    ) -> bool {
        slot.borrow()
            .as_ref()
            .is_some_and(|actual| actual.id == self.id && Rc::ptr_eq(actual, self))
    }

    /// Preserve an irreversible rejection. Revoking an old receipt must not
    /// reset a real successor input's Known selection.
    pub(in crate::native_surface) fn reject(&self) {
        self.revoked.set(true);
        self.committed.revoke_pending_selection();
    }

    /// Both local rejection and original committed-input authority remain live.
    pub(in crate::native_surface) fn current(&self) -> bool {
        !self.revoked.get() && self.committed.current()
    }

    /// Check the same owner around a reentrant Native operation. A changed owner
    /// leaves rejection permanent rather than blessing a returned stale value.
    pub(in crate::native_surface) fn native_step<T>(
        &self,
        work: impl FnOnce() -> T,
    ) -> Result<T, NativeMarkdownFoldError> {
        if !self.current() {
            self.reject();
            return Err(NativeMarkdownFoldError::Stale);
        }
        let result = work();
        if !self.current() {
            self.reject();
            return Err(NativeMarkdownFoldError::Stale);
        }
        Ok(result)
    }

    /// Compare raw values only with coordinates fixed before the first
    /// notification. Old before may exceed new EOF, but grants no new text read.
    pub(in crate::native_surface) fn observe(
        &self,
        present: bool,
        count: usize,
        raw: NSRange,
    ) -> bool {
        if !self.current() || !present || count != 1 || self.source_hits(raw) == 0 {
            self.reject();
            return false;
        }
        true
    }

    #[cfg(feature = "native-markdown-runtime-probe")]
    /// Record bounded diagnostic progress without changing input authority.
    pub(in crate::native_surface) fn record_read(&self) {
        if let Some(next) = self.reads.get().checked_add(1) {
            self.reads.set(next);
        }
    }

    /// Admit one synchronous sample. Recursive observation permanently rejects
    /// the original owner even if a getter later returns the earlier value.
    pub(in crate::native_surface) fn begin_sample(
        self: &Rc<Self>,
    ) -> Option<NativeMarkdownPendingSelectionSampleGuard> {
        if !self.current() || self.sampling.replace(true) {
            self.reject(); // Neither recursive getters nor an A-after-B return can revive this receipt.
            return None;
        }
        Some(NativeMarkdownPendingSelectionSampleGuard {
            owner: Rc::clone(self),
        })
    }

    /// Match only the predeclared finite sources allowed in this actual phase.
    /// Diagnostic hit bits never add a new selection or source-read permission.
    pub(in crate::native_surface) fn source_hits(&self, raw: NSRange) -> u16 {
        let mut hits = 0_u16;
        for (index, range) in self.ranges.iter().enumerate() {
            if index == 7
                && !matches!(
                    self.phase.get(),
                    NativeMarkdownPendingSelectionPhase::EndEditing
                        | NativeMarkdownPendingSelectionPhase::SuperclassTail
                )
            {
                continue; // Original EOF is valid only for EndEditing and its continuous installation tail.
            }
            if index == 8
                && !matches!(
                    self.phase.get(),
                    NativeMarkdownPendingSelectionPhase::SuperclassTail
                )
            {
                continue; // Delete-only EOF belongs exclusively to the actual input superclass tail.
            }
            if *range == Some(raw) {
                // Slots 0..6 retain original sources; 7 is pre-Native EOF; 8 is
                // EOF after original deletion and before candidate insertion.
                hits |= 1 << index;
            }
        }
        hits
    }
}

/// Getter/pool-drain reentry leaves rejection sticky. This local guard restores
/// sampling availability only, never the revoked authority.
pub(in crate::native_surface) struct NativeMarkdownPendingSelectionSampleGuard {
    owner: Rc<NativeMarkdownPendingSelectionOwner>,
}
impl Drop for NativeMarkdownPendingSelectionSampleGuard {
    fn drop(&mut self) {
        self.owner.sampling.set(false);
    }
}

#[cfg(feature = "native-markdown-runtime-probe")]
pub(in crate::native_surface) fn native_markdown_pending_selection_trace_enabled() -> bool {
    std::env::var_os("OMNI_NATIVE_PROBE_PENDING_SELECTION_TRACE").is_some()
}

#[cfg(not(feature = "native-markdown-runtime-probe"))]
pub(in crate::native_surface) fn native_markdown_pending_selection_trace_enabled() -> bool {
    false // Ordinary releases read no probe environment variable and emit no per-notification trace.
}

impl PieceBackedMarkdownTextStorage {
    /// Install an owner after pure commit. Construction revocation still keeps
    /// its rejection marker rather than reverting to ordinary recapture.
    pub(in crate::native_surface) fn begin_pending_selection(
        &self,
        committed: &Rc<NativeMarkdownCommittedSelection>,
        sources: NativeMarkdownPendingSelectionSources,
    ) -> Rc<NativeMarkdownPendingSelectionOwner> {
        if let Some(existing) = self.pending_selection_for(committed) {
            return existing;
        }
        // Exact new length uses the original commit and Weak document identity.
        // An unknown head in a range receipt does not replace those length facts.
        let new_len = self
            .document()
            .and_then(|document| committed.current_utf16_len_for(&document));
        let new_eof = new_len.map(|length| NSRange::new(length, 0));
        let original_eof_sources = new_len.and_then(|length| {
            native_markdown_pending_selection_eof_sources(
                committed.native_before_utf16_len(),
                length,
                sources,
            )
        });
        // Character/fold publication is complete before the first Native
        // notification or pool drain. Length capture verifies original
        // generation/full selection/serial, without deriving EOF from raw values.
        let id = committed.selection_serial(); // Reuse the original finite serial without a new fallible post-commit counter.
        let selection = committed.selection;
        let after = NSRange::new(
            selection.anchor_utf16.min(selection.head_utf16),
            selection.anchor_utf16.abs_diff(selection.head_utf16),
        );
        let owner = Rc::new(NativeMarkdownPendingSelectionOwner {
            id,
            committed: Rc::clone(committed),
            phase: Cell::new(NativeMarkdownPendingSelectionPhase::Published),
            source: sources.source,
            sampling: Cell::new(false),
            revoked: Cell::new(new_eof.is_none() || original_eof_sources.is_none()),
            #[cfg(feature = "native-markdown-runtime-probe")]
            reads: Rc::clone(&self.ivars().pending_selection_reads),
            #[cfg(feature = "native-markdown-runtime-probe")]
            fault: self
                .ivars()
                .pending_selection_fault
                .borrow()
                .as_ref()
                .map(Rc::downgrade),
            ranges: [
                Some(committed.native_before_range()),
                Some(after),
                sources.replacement,
                sources.caret,
                sources.marked_before,
                sources.marked_after,
                new_eof,
                original_eof_sources.map(|(old_eof, _)| old_eof),
                original_eof_sources.and_then(|(_, deleted_eof)| deleted_eof),
            ],
        });
        let previous = self
            .ivars()
            .pending_selection
            .replace(Some(Rc::clone(&owner)));
        drop(previous); // The slot borrow has ended; an old guard cannot clear this successor id.
        if owner.revoked.get() {
            owner.reject();
        }
        if native_markdown_pending_selection_trace_enabled() {
            eprintln!(
                "omni_pending event=begin id={id} source={} current={} ranges={:?}",
                owner.source,
                committed.current(),
                owner.ranges
            );
        }
        owner
    }

    /// Borrow only the pending owner of this exact committed receipt; an equal
    /// selection in a successor notification is insufficient.
    pub(in crate::native_surface) fn pending_selection_for(
        &self,
        committed: &Rc<NativeMarkdownCommittedSelection>,
    ) -> Option<Rc<NativeMarkdownPendingSelectionOwner>> {
        self.ivars()
            .pending_selection
            .borrow()
            .as_ref()
            .filter(|owner| Rc::ptr_eq(&owner.committed, committed))
            .map(Rc::clone)
    }

    /// Label the current synchronous phase only while original id and Rc match.
    pub(in crate::native_surface) fn pending_selection_phase(
        &self,
        owner: &Rc<NativeMarkdownPendingSelectionOwner>,
        phase: NativeMarkdownPendingSelectionPhase,
    ) {
        if self
            .ivars()
            .pending_selection
            .borrow()
            .as_ref()
            .is_some_and(|current| current.id == owner.id && Rc::ptr_eq(current, owner))
        {
            owner.phase.set(phase); // No Native call or additional selection authority is introduced by this label.
        }
    }

    /// Advance phase for the same original committed receipt, without capturing
    /// another owner if its pending slot has already ended.
    pub(in crate::native_surface) fn pending_selection_phase_for(
        &self,
        committed: &Rc<NativeMarkdownCommittedSelection>,
        phase: NativeMarkdownPendingSelectionPhase,
    ) {
        if let Some(owner) = self.pending_selection_for(committed) {
            self.pending_selection_phase(&owner, phase);
        }
    }

    /// Return only this exact original slot before dropping its receipt outside
    /// the RefCell borrow. A reentrant successor remains independently owned.
    pub(in crate::native_surface) fn finish_pending_selection(
        &self,
        owner: &Rc<NativeMarkdownPendingSelectionOwner>,
    ) {
        let removed = take_pending_selection_owner(&self.ivars().pending_selection, owner);
        if removed.is_some() && native_markdown_pending_selection_trace_enabled() {
            eprintln!(
                "omni_pending event=end id={} source={} phase={:?} current={}",
                owner.id,
                owner.source,
                owner.phase.get(),
                owner.committed.current()
            );
        }
        drop(removed);
    }
}

/// Local guard on the canonical notification path. Its destructor cannot take
/// a successor owner occupying the same shared slot.
pub(in crate::native_surface) struct NativeMarkdownPendingSelectionGuard {
    pub(in crate::native_surface) slot: Rc<NativeMarkdownPendingSelectionSlot>,
    pub(in crate::native_surface) owner: Rc<NativeMarkdownPendingSelectionOwner>,
}
impl Drop for NativeMarkdownPendingSelectionGuard {
    fn drop(&mut self) {
        let removed = take_pending_selection_owner(&self.slot, &self.owner);
        if removed.is_some() && native_markdown_pending_selection_trace_enabled() {
            eprintln!(
                "omni_pending event=end id={} source={} phase={:?} current={}",
                self.owner.id,
                self.owner.source,
                self.owner.phase.get(),
                self.owner.current()
            );
        }
        drop(removed); // Release outside the slot borrow, without strong storage/Native/document ownership.
    }
}

#[cfg(test)]
#[path = "pending_selection/tests.rs"]
mod tests;
