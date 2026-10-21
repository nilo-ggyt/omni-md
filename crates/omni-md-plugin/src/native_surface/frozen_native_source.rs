//! Encapsulate one immutable Source for the explicit frozen-value diagnostic graph. The snapshot
//! never escapes; its actual resources drop before the family's final Source claim settles.

use std::ops::Range;
use std::sync::{Arc, Weak};

use omni_markdown_core::{
    MarkdownDocumentBuffer, MarkdownDocumentBufferError, MarkdownTextSnapshot,
};

use super::frozen_value_family::{FrozenValueClaim, FrozenValueFamily, FrozenValueFamilyError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FrozenReadKind {
    Character,
    Bulk,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Carry fixed scalars only. Invalid requests retain their original arguments and checked results,
/// rather than being truncated into another valid window.
pub(super) struct FrozenReadRequest {
    pub(super) kind: FrozenReadKind,
    pub(super) snapshot_utf16_len: usize,
    pub(super) receiver_start: usize,
    pub(super) receiver_length: usize,
    pub(super) local_start: usize,
    pub(super) requested_length: usize,
    pub(super) checked_global_start: Option<usize>,
    pub(super) checked_global_end: Option<usize>,
    pub(super) receiver_valid: bool,
    pub(super) request_valid: bool,
}

/// The diagnostic sink may observe fixed facts only: no captured owner, Native calls, or panic.
/// The sink owns that restriction; this function pointer grants no live editing or Source authority.
pub(super) type FrozenReadHook = fn(FrozenReadRequest);

struct SourceData {
    // Fields drop in declaration order. The snapshot returns its mmap/root/edit-segment ownership
    // before this last Source claim reports payload destruction to the family.
    snapshot: MarkdownTextSnapshot,
    generation: u64,
    read_hook: Option<FrozenReadHook>,
    claim: FrozenValueClaim,
}

#[derive(Clone)]
/// Clone shares the same complete SourceData, without copying a bare snapshot or adding another
/// Source claim for the same payload.
pub(super) struct FrozenNativeSource {
    data: Arc<SourceData>,
}

#[derive(Clone)]
pub(super) struct FrozenNativeSourceWeak {
    data: Weak<SourceData>,
}

/// Created only after notifying the diagnostic hook and validating the entire request. The
/// private snapshot borrow cannot escape through this read token.
pub(super) struct FrozenNativeRead<'a> {
    snapshot: &'a MarkdownTextSnapshot,
    start: usize,
    length: usize,
}

impl FrozenNativeSource {
    pub(super) fn capture(
        document: &MarkdownDocumentBuffer,
        family: &Arc<FrozenValueFamily>,
        read_hook: Option<FrozenReadHook>,
    ) -> Result<Self, FrozenValueFamilyError> {
        // Reject sealed-source admission first; never create a bare snapshot and retroactively
        // attach it to the original family's authority.
        let claim = family.claim_source()?;
        let snapshot = document.text_snapshot();
        let generation = document.generation();
        Ok(Self {
            data: Arc::new(SourceData {
                snapshot,
                generation,
                read_hook,
                claim,
            }),
        })
    }

    pub(super) fn downgrade(&self) -> FrozenNativeSourceWeak {
        // The live diagnostic cache keeps only this original allocation's weak identity.
        FrozenNativeSourceWeak {
            data: Arc::downgrade(&self.data),
        }
    }

    pub(super) fn generation(&self) -> u64 {
        self.data.generation // Capture-time generation only; it is neither cross-document identity nor renewed live authority.
    }

    pub(super) fn utf16_len(&self) -> usize {
        self.data.snapshot.utf16_len()
    }

    pub(super) fn text_checking_paragraph_range(
        &self,
        range: Range<usize>,
    ) -> Result<Range<usize>, MarkdownDocumentBufferError> {
        // Query only the frozen tree's paragraph metadata. Expose neither its bare snapshot nor
        // a full-document String or permission to access a live document.
        self.data.snapshot.text_checking_paragraph_range(range)
    }

    pub(super) fn claim_string_value(&self) -> Result<FrozenValueClaim, FrozenValueFamilyError> {
        // Derivation requires this still-live Source claim, including after new-source sealing.
        self.data.claim.claim_string_value()
    }

    pub(super) fn request_read(
        &self,
        kind: FrozenReadKind,
        receiver_start: usize,
        receiver_length: usize,
        local_start: usize,
        requested_length: usize,
    ) -> Result<FrozenNativeRead<'_>, MarkdownDocumentBufferError> {
        // Validate receiver-local and snapshot-global coordinates independently with checked
        // arithmetic; sharing the snapshot never permits reading beyond a substring receiver.
        let snapshot_utf16_len = self.utf16_len();
        let receiver_end = receiver_start.checked_add(receiver_length);
        let local_end = local_start.checked_add(requested_length);
        let checked_global_start = receiver_start.checked_add(local_start);
        let checked_global_end =
            checked_global_start.and_then(|start| start.checked_add(requested_length));
        let receiver_valid = receiver_end.is_some_and(|end| end <= snapshot_utf16_len);
        let request_valid = receiver_valid
            && local_end.is_some_and(|end| end <= receiver_length)
            && checked_global_end.is_some_and(|end| end <= snapshot_utf16_len);
        if let Some(hook) = self.data.read_hook {
            // Invalid requests report their original facts before the caller raises an exception.
            // Pass only a function pointer and scalars, without document or native business borrows.
            hook(FrozenReadRequest {
                kind,
                snapshot_utf16_len,
                receiver_start,
                receiver_length,
                local_start,
                requested_length,
                checked_global_start,
                checked_global_end,
                receiver_valid,
                request_valid,
            });
        }
        if !request_valid {
            // Issue neither a read token nor an output slice. An invalid receiver request cannot
            // borrow neighboring characters merely because they exist in the same Source.
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        Ok(FrozenNativeRead {
            snapshot: &self.data.snapshot,
            start: checked_global_start.ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?,
            length: requested_length,
        })
    }
}

impl FrozenNativeSourceWeak {
    pub(super) fn upgrade(&self) -> Option<FrozenNativeSource> {
        // Upgrade retains the same Data/Source claim; even after sealing, a Weak cannot construct
        // another snapshot or revive a payload whose actual strong ownership already ended.
        self.data.upgrade().map(|data| FrozenNativeSource { data })
    }
}

impl FrozenNativeRead<'_> {
    #[cfg(test)]
    pub(super) fn global_start(&self) -> usize {
        self.start
    }

    pub(super) fn length(&self) -> usize {
        self.length
    }

    pub(super) fn code_unit(&self) -> Result<u16, MarkdownDocumentBufferError> {
        if self.length != 1 {
            // The character primitive consumes exactly one UTF-16 unit; it cannot shorten a bulk
            // request into one unit and present that as completion of the original request.
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        self.snapshot.utf16_code_unit_at(self.start)
    }

    pub(super) fn copy_to(
        &self,
        destination: &mut [u16],
    ) -> Result<(), MarkdownDocumentBufferError> {
        if destination.len() != self.length {
            // The output must cover the entire original request, without a short buffer,
            // truncation, or substitution of another range.
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        self.snapshot.copy_utf16_code_units(self.start, destination)
    }
}
