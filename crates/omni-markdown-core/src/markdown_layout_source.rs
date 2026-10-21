//! Layout algorithms share one frozen text payload with fixed capture identity
//! and generation. Bounded reads grant no native live-document mutation authority.
use std::{
    hash::{Hash, Hasher},
    io::Write,
    ops::Range,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

use crate::{MarkdownDocumentBuffer, MarkdownDocumentBufferError, MarkdownTextSnapshot};

#[cfg(feature = "paged-line-compose")]
pub(crate) mod folds;

#[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
mod table_row;
#[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
pub use table_row::VerifiedMarkdownLayoutTableRow;
#[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
mod root_leaf_boundary;
#[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
pub use root_leaf_boundary::VerifiedMarkdownRootLeafStyleBoundary;
#[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
pub(crate) mod table_rows;

#[cfg(feature = "s2-table-window-prototype")]
mod cell_draft;
#[cfg(feature = "s2-table-window-prototype")]
mod table_cell;
#[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
pub use table_cell::{
    MarkdownTableCellDisplayPreparation, MarkdownTableCellDisplayPreparationStep,
    MarkdownTableCellDisplayResourceObserver, MarkdownTableCellDisplayResources,
    MarkdownTableCellDisplaySource, MarkdownTableCellVisibleSourceRanges,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Coordinate domain of a layout source. Read-only cells have no canonical
/// input authority, and active drafts cannot edit body text using local offsets.
pub enum MarkdownLayoutTextSourceDomain {
    Canonical,
    #[cfg(feature = "s2-table-window-prototype")]
    ReadOnlyCell,
    #[cfg(feature = "s2-table-window-prototype")]
    ActiveCellDraft,
}

enum SourceDomain {
    Canonical,
    #[cfg(feature = "s2-table-window-prototype")]
    // Read-only cells own their structural metadata separately, avoiding a
    // largest-cell payload cost for every ordinary body capture.
    ReadOnlyCell(Box<crate::markdown_document_buffer::VerifiedMarkdownTableCellSource>),
    #[cfg(feature = "s2-table-window-prototype")]
    ActiveCellDraft(crate::markdown_document_buffer::MarkdownCellDraftSourceAuthority),
}

/// Per-read text limit shared by layout algorithms. Longer pieces or soft lines
/// never enlarge the permitted lease.
pub const MARKDOWN_LAYOUT_SOURCE_BYTES: usize = 4096;

/// Contains the original frozen snapshot and fixed aggregates only. Cloning
/// shares its `Arc`, without recapturing or copying document text.
#[derive(Clone)]
pub struct MarkdownLayoutTextSource(Arc<Payload>);
struct Payload {
    // Fields drop in declaration order. The final marker publishes release
    // only after this payload has actually returned its snapshot ownership.
    snapshot: MarkdownTextSnapshot,
    generation: u64,
    bytes: usize,
    utf16: usize,
    domain: SourceDomain,
    dropped: PayloadDrop,
}
struct PayloadDrop(Arc<AtomicBool>);
impl Drop for PayloadDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// Identifies the original immutable payload without retaining its snapshot,
/// mapping, edit segments, live document, or history.
#[derive(Clone)]
pub struct MarkdownLayoutTextAuthority {
    source: Weak<Payload>,
}

impl PartialEq for MarkdownLayoutTextAuthority {
    fn eq(&self, other: &Self) -> bool {
        // Weak control-block identity survives payload destruction, so equal text
        // or a reused address cannot masquerade as the original capture.
        Weak::ptr_eq(&self.source, &other.source)
    }
}
impl Eq for MarkdownLayoutTextAuthority {}
impl PartialOrd for MarkdownLayoutTextAuthority {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for MarkdownLayoutTextAuthority {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // This total order sorts original payload keys only. It represents neither
        // text order, newer generations, nor active input authority.
        // Weak ownership prevents control-block reuse, keeping order consistent
        // with equality/hash. Same-source text positions compare UTF-16 separately.
        self.source.as_ptr().cmp(&other.source.as_ptr())
    }
}
impl Hash for MarkdownLayoutTextAuthority {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Finite native-position keys hash identity only, exposing no text. Equal
        // keys always use the same original payload address.
        self.source.as_ptr().hash(state);
    }
}

/// Retains a fixed destruction marker only. It does not retain source storage or
/// treat weak-reference expiry as proof that all payload fields have dropped.
#[derive(Clone)]
pub struct MarkdownLayoutTextSourceObserver {
    dropped: Arc<AtomicBool>,
}
impl MarkdownLayoutTextSourceObserver {
    /// Proves only that this payload returned its snapshot. Other snapshots or
    /// native values may still own the shared mapping.
    #[must_use]
    pub fn payload_released(&self) -> bool {
        self.dropped.load(Ordering::Acquire)
    }
}

impl MarkdownLayoutTextSource {
    /// Captures generation and the original snapshot under one immutable document
    /// borrow, without reentrant calls or a whole-text scan.
    ///
    /// Later edits, undo/redo, and document destruction cannot alter this value.
    /// Generation records capture-time facts only;
    /// native consumption must still verify the original owner and current generation.
    #[must_use]
    pub fn capture(document: &MarkdownDocumentBuffer) -> Self {
        Self(Arc::new(Payload {
            snapshot: document.text_snapshot(),
            generation: document.generation(),
            bytes: document.byte_len(),
            utf16: document.utf16_len(),
            domain: SourceDomain::Canonical,
            dropped: PayloadDrop(Arc::new(AtomicBool::new(false))),
        }))
    }
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.0.generation
    }
    /// Reports the domain from its actual construction path. Absence of a draft
    /// ticket does not turn a local read-only cell into a full canonical source.
    #[must_use]
    pub fn domain(&self) -> MarkdownLayoutTextSourceDomain {
        match &self.0.domain {
            SourceDomain::Canonical => MarkdownLayoutTextSourceDomain::Canonical,
            #[cfg(feature = "s2-table-window-prototype")]
            SourceDomain::ReadOnlyCell(_) => MarkdownLayoutTextSourceDomain::ReadOnlyCell,
            #[cfg(feature = "s2-table-window-prototype")]
            SourceDomain::ActiveCellDraft(_) => MarkdownLayoutTextSourceDomain::ActiveCellDraft,
        }
    }
    #[must_use]
    pub fn byte_len(&self) -> usize {
        self.0.bytes
    }
    #[must_use]
    pub fn utf16_len(&self) -> usize {
        self.0.utf16
    }
    /// Even empty documents have distinct payload allocations. Equal text and
    /// generation cannot impersonate the same source.
    #[must_use]
    pub fn authority(&self) -> MarkdownLayoutTextAuthority {
        MarkdownLayoutTextAuthority {
            source: Arc::downgrade(&self.0),
        }
    }
    #[must_use]
    pub fn resource_observer(&self) -> MarkdownLayoutTextSourceObserver {
        MarkdownLayoutTextSourceObserver {
            dropped: Arc::clone(&self.0.dropped.0),
        }
    }
    /// Converts endpoints in this payload, checking real source health even at
    /// endpoints or cache hits. It never recaptures or reads the entire paragraph.
    ///
    /// # Errors
    /// Preserves mapping, range, and surrogate-boundary failures. Only a bounded
    /// leaf is located, and no document-text borrow escapes.
    #[cfg(any(feature = "paged-shaping", test))]
    pub fn byte_offset_for_utf16(
        &self,
        position: usize,
    ) -> Result<usize, MarkdownDocumentBufferError> {
        self.0.snapshot.byte_offset_for_utf16(position)
    }
    /// Exact character-domain predicate on the frozen payload, without another
    /// document capture or copying an entire item into temporary memory.
    /// The owner still checks the same authority around this operation; endpoint
    /// conversion and this predicate share the original error type.
    #[cfg(any(feature = "paged-shaping-frontier", test))]
    pub(crate) fn is_printable_ascii_range(
        &self,
        range: Range<usize>,
    ) -> Result<bool, MarkdownDocumentBufferError> {
        self.0.snapshot.is_printable_ascii_range(range)
    }

    /// Begins the script suffix proof using this payload's frozen value only,
    /// without another source capture or endpoint read.
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn begin_printable_ascii_suffix(
        &self,
        range: Range<usize>,
    ) -> Result<
        crate::markdown_document_buffer::FrozenPrintableAsciiSuffix,
        MarkdownDocumentBufferError,
    > {
        self.0.snapshot.begin_printable_ascii_suffix(range)
    }

    /// Charges bytes, scalars, and directory records separately against this
    /// quantum, preserving actual source errors.
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn advance_printable_ascii_suffix(
        &self,
        cursor: &mut crate::markdown_document_buffer::FrozenPrintableAsciiSuffix,
        bytes: usize,
        scalars: usize,
        metadata: usize,
    ) -> Result<
        crate::markdown_document_buffer::FrozenPrintableAsciiSuffixStep,
        MarkdownDocumentBufferError,
    > {
        self.0
            .snapshot
            .advance_printable_ascii_suffix(cursor, bytes, scalars, metadata)
    }

    /// Same-source tree cursor for the dense grapheme consumer. It creates no
    /// second source/reader and exposes no raw document text.
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn begin_ascii_word_run(
        &self,
        position: usize,
        forward: bool,
    ) -> Result<crate::markdown_document_buffer::MarkdownAsciiWordRun, MarkdownDocumentBufferError>
    {
        self.0
            .snapshot
            .begin_ascii_word_run(position, forward, self.authority())
    }

    /// Equal text or shared roots cannot transfer a cursor between sources. Source
    /// failure precedes identity rejection and is rechecked after the actual read.
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn advance_ascii_word_run(
        &self,
        run: &mut crate::markdown_document_buffer::MarkdownAsciiWordRun,
        bytes: usize,
        scalars: usize,
        metadata: usize,
    ) -> Result<
        crate::markdown_document_buffer::MarkdownAsciiWordStep,
        crate::MarkdownLineComposeError,
    > {
        self.health()?;
        if !self.permits(run.authority()) {
            return Err(crate::MarkdownLineComposeError::SourceMismatch);
        }
        let result = self
            .0
            .snapshot
            .advance_ascii_word_run(run, bytes, scalars, metadata);
        self.health()?;
        result.map_err(Into::into)
    }
    /// Exact three-state hint for the same frozen complete source. Script consumers
    /// still verify original authority around pure-Core handoff.
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn printable_ascii_has_letters(
        &self,
    ) -> Result<Option<bool>, MarkdownDocumentBufferError> {
        self.0.snapshot.printable_ascii_has_letters()
    }

    /// Reads the same frozen payload's sticky I/O state. Weak identity or a stale
    /// index hit cannot substitute for this health gate.
    pub(crate) fn health(&self) -> Result<(), MarkdownDocumentBufferError> {
        self.0.snapshot.original_backing_health()
    }
    /// Cancellation checks retain atomic facts only, avoiding a strong frozen-text
    /// owner merely to observe an original-source I/O failure.
    #[must_use]
    pub fn original_backing_observer(&self) -> Option<crate::MarkdownOriginalBackingObserver> {
        self.0.snapshot.original_backing_observer()
    }

    /// Streams a body/cell UTF-16 range as original UTF-8 pieces without copying
    /// an entire paragraph or cell into memory.
    ///
    /// # Errors
    /// Checks source health, range order, and both scalar endpoints before the
    /// first writer call. Even an empty range passes the health gate.
    /// Writer failure may leave partial output but changes neither source nor its
    /// input state. Original lease cleanup and cell-content revocation
    /// propagate through the same snapshot gate; actual source failure takes
    /// precedence over writer failure.
    pub fn write_utf16_range_to(
        &self,
        range: Range<usize>,
        destination: &mut impl Write,
    ) -> Result<(), MarkdownDocumentBufferError> {
        self.health()?;
        if range.start > range.end {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        } // Never clamp or reverse a user selection; snapshot conversion validates both endpoints.
        let start = self.0.snapshot.byte_offset_for_utf16(range.start)?;
        let end = self.0.snapshot.byte_offset_for_utf16(range.end)?;
        // The snapshot visitor leases intersecting original pieces only, at most
        // 4 KiB per callback. Large selections never enlarge one source lease.
        // Streaming intentionally avoids the single-query layout API; explicit
        // large copies need neither a selected-text string nor repeated output reads.
        let result = self.0.snapshot.visit_byte_range(start..end, |bytes| {
            destination
                .write_all(bytes)
                .map_err(|_| MarkdownDocumentBufferError::WriteUnavailable)
        });
        self.health()?; // Recheck even after the final writer; no later leaf may exist to discover source failure.
        result
    }

    /// Shares the original frozen value with bidi and later paging stages, adding
    /// an `Arc` reference without recapturing the document or copying text.
    #[cfg(feature = "paged-layout-breaks")]
    pub(crate) fn snapshot_for_bidi(&self) -> MarkdownTextSnapshot {
        self.0.snapshot.clone()
    }

    pub(crate) fn permits(&self, authority: &MarkdownLayoutTextAuthority) -> bool {
        Weak::ptr_eq(&authority.source, &Arc::downgrade(&self.0))
    }

    /// Visits bounded original pieces without copying text. Each algorithm's own
    /// continuation state handles byte endpoints inside UTF-8 scalars.
    pub(crate) fn visit_byte_range(
        &self,
        range: Range<usize>,
        visit: impl FnMut(&[u8]) -> Result<(), MarkdownDocumentBufferError>,
    ) -> Result<(), MarkdownDocumentBufferError> {
        self.health()?;
        if range.start > range.end || range.end > self.byte_len() {
            // Reversed or out-of-source coordinates are range errors, not a reason
            // to retry with a larger read budget.
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        if range.len() > MARKDOWN_LAYOUT_SOURCE_BYTES {
            // The budget is mandatory. Reject an oversized request before borrowing
            // any document bytes.
            return Err(MarkdownDocumentBufferError::MaterializationLimitExceeded);
        }
        self.0.snapshot.visit_byte_range(range, visit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streamed_body_range_keeps_original_and_edit_pieces_after_document_release() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("跨片来源.md");
        let original = format!("{}\r\n中😀e\u{301}{}尾", "a".repeat(4095), "b".repeat(4193));
        std::fs::write(&path, &original).unwrap();
        let mut document = MarkdownDocumentBuffer::open(&path, 65536).unwrap();
        document
            .replace_utf16_range(
                4097..4098,
                "编辑😀\r\n",
                crate::MarkdownTextSelection::caret(4103),
            )
            .unwrap();
        let expected = original.replacen('中', "编辑😀\r\n", 1);
        let units = expected.encode_utf16().collect::<Vec<_>>();
        let range = 4091..8198;
        let expected = String::from_utf16(&units[range.clone()]).unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let observer = source.original_backing_observer().unwrap();
        let generation = document.generation();
        let selection = document.selection();
        let mut bytes = Vec::new();
        source
            .write_utf16_range_to(range.clone(), &mut bytes)
            .unwrap();
        assert_eq!(bytes, expected.as_bytes());
        assert_eq!(document.generation(), generation);
        assert_eq!(document.selection(), selection);
        assert!(document.is_dirty() && document.can_undo() && !document.can_redo());
        drop(document); // Frozen body source retains bytes, never the active document or its history.
        directory.close().unwrap();
        bytes.clear();
        source.write_utf16_range_to(range, &mut bytes).unwrap();
        assert_eq!(bytes, expected.as_bytes());
        drop(source);
        assert!(observer.unmap_succeeded() && observer.mapped_bytes() == 0 && !observer.failed());
    }

    #[test]
    fn streamed_source_rejects_every_invalid_scalar_endpoint_before_writer_io() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), "A😀\r\n中").unwrap();
        let document = MarkdownDocumentBuffer::open(file.path(), 4096).unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        for (range, expected) in [
            (2..3, MarkdownDocumentBufferError::NotUnicodeBoundary),
            (0..2, MarkdownDocumentBufferError::NotUnicodeBoundary),
            (2..2, MarkdownDocumentBufferError::NotUnicodeBoundary),
            (
                Range { start: 4, end: 1 },
                MarkdownDocumentBufferError::RangeOutsideDocument,
            ),
            (0..7, MarkdownDocumentBufferError::RangeOutsideDocument),
        ] {
            let mut bytes = b"existing".to_vec();
            assert_eq!(
                source.write_utf16_range_to(range, &mut bytes),
                Err(expected)
            );
            assert_eq!(bytes, b"existing"); // Invalid endpoints must be rejected before any real writer output.
        }
    }

    #[test]
    fn source_readonly_fd_failure_preserves_body_selection_and_both_history_directions() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), "原文😀\r\n尾").unwrap();
        let mut document = MarkdownDocumentBuffer::open(file.path(), 4096).unwrap();
        document
            .replace_utf16_range(0..2, "修改", crate::MarkdownTextSelection::caret(2))
            .unwrap();
        document
            .replace_utf16_range(0..2, "再改", crate::MarkdownTextSelection::caret(2))
            .unwrap();
        document.undo_text_edit().unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let generation = document.generation();
        let selection = document.selection();
        let revision = document.current_revision().unwrap();
        let mut readonly = std::fs::File::open("/dev/null").unwrap();
        assert_eq!(
            source.write_utf16_range_to(0..source.utf16_len(), &mut readonly),
            Err(MarkdownDocumentBufferError::WriteUnavailable)
        );
        assert_eq!(document.generation(), generation);
        assert_eq!(document.selection(), selection);
        assert_eq!(document.current_revision().unwrap(), revision);
        assert!(document.is_dirty() && document.can_undo() && document.can_redo());
        let mut bytes = Vec::new();
        source
            .write_utf16_range_to(0..source.utf16_len(), &mut bytes)
            .unwrap();
        assert_eq!(bytes, "修改😀\r\n尾".as_bytes());
    }

    #[test]
    fn final_edit_piece_writer_failure_cannot_hide_actual_source_reclaim_eio() {
        struct ReclaimingWriter<'a> {
            document: &'a MarkdownDocumentBuffer,
            readonly: std::fs::File,
        }
        impl Write for ReclaimingWriter<'_> {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                // The destination remains a real read-only descriptor. First make the
                // actual reclamation of an already-read original page report EIO.
                self.document.fail_next_original_reclaim_for_test();
                assert_eq!(
                    self.document.reclaim_original_read_windows_for_test(),
                    Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                        libc::EIO
                    ))
                );
                self.readonly.write(bytes)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.readonly.flush()
            }
        }
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), b"x".repeat(128 * 1024)).unwrap();
        let mut document = MarkdownDocumentBuffer::open(file.path(), 256 * 1024).unwrap();
        document
            .insert_utf16(0, "编辑", crate::MarkdownTextSelection::caret(2))
            .unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        source
            .visit_byte_range(64 * 1024..64 * 1024 + 1, |_| Ok(()))
            .unwrap();
        let observer = source.original_backing_observer().unwrap();
        let generation = document.generation();
        let selection = document.selection();
        {
            let mut writer = ReclaimingWriter {
                document: &document,
                readonly: std::fs::File::open("/dev/null").unwrap(),
            };
            // The selection contains edited pieces only. Its last leaf has no
            // original lease, but source health must still precede writer failure.
            assert_eq!(
                source.write_utf16_range_to(0..2, &mut writer),
                Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                    libc::EIO
                ))
            );
        }
        assert_eq!(document.generation(), generation);
        assert_eq!(document.selection(), selection);
        assert!(document.is_dirty() && document.can_undo() && !document.can_redo());
        drop((source, document));
        assert!(observer.unmap_succeeded() && observer.mapped_bytes() == 0);
        assert_eq!(observer.first_errno(), Some(libc::EIO));
    }

    #[test]
    fn printable_domain_uses_same_frozen_payload_and_excludes_ascii_controls() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source-domain.md");
        std::fs::write(&path, b"printable ~").unwrap();
        let mut document = MarkdownDocumentBuffer::open(&path, 65536).unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let authority = source.authority();
        assert_eq!(
            source.is_printable_ascii_range(0..source.utf16_len()),
            Ok(true)
        );
        document
            .insert_utf16(2, "\t", crate::MarkdownTextSelection::caret(3))
            .unwrap();
        let edited = MarkdownLayoutTextSource::capture(&document);
        // Equal byte/UTF-16 lengths can still contain C0 controls. A separate
        // capture cannot replace the original payload's exact domain facts.
        assert_eq!(edited.byte_len(), edited.utf16_len());
        assert_eq!(
            edited.is_printable_ascii_range(0..edited.utf16_len()),
            Ok(false)
        );
        assert!(source.permits(&authority) && !edited.permits(&authority));
        assert_eq!(
            source.is_printable_ascii_range(0..source.utf16_len()),
            Ok(true)
        );
    }

    #[test]
    fn bounded_visit_rejects_invalid_requests_before_exposing_any_piece() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("finite-source.md");
        std::fs::write(&path, b"x".repeat(8192)).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 16384).unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let mut calls = 0;
        assert_eq!(
            source.visit_byte_range(0..4097, |_| {
                calls += 1;
                Ok(())
            }),
            Err(MarkdownDocumentBufferError::MaterializationLimitExceeded)
        );
        assert_eq!(
            source.visit_byte_range(Range { start: 1, end: 0 }, |_| {
                calls += 1;
                Ok(())
            }),
            Err(MarkdownDocumentBufferError::RangeOutsideDocument)
        );
        assert_eq!(
            source.visit_byte_range(8192..8193, |_| {
                calls += 1;
                Ok(())
            }),
            Err(MarkdownDocumentBufferError::RangeOutsideDocument)
        );
        source
            .visit_byte_range(8192..8192, |_| {
                calls += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(calls, 0);
        assert_eq!(source.byte_offset_for_utf16(0), Ok(0));
        assert_eq!(source.byte_offset_for_utf16(8192), Ok(8192));
        assert_eq!(
            source.byte_offset_for_utf16(8193),
            Err(MarkdownDocumentBufferError::RangeOutsideDocument)
        );
        let mut bytes = 0;
        source
            .visit_byte_range(4095..8191, |piece| {
                assert!(piece.len() <= 4096);
                bytes += piece.len();
                Ok(())
            })
            .unwrap();
        assert_eq!(bytes, 4096);
        let clone = source.clone();
        let other = MarkdownLayoutTextSource::capture(&document);
        let authority = source.authority();
        assert!(authority == clone.authority());
        assert!(authority != other.authority());
        let mut identities = std::collections::HashSet::new();
        identities.insert(authority.clone());
        identities.insert(clone.authority());
        identities.insert(other.authority());
        assert_eq!(identities.len(), 2); // Hash follows weak identity; independent equal-text captures keep separate keys.
        let ordered: std::collections::BTreeSet<_> = identities.into_iter().collect();
        assert_eq!(ordered.len(), 2); // Cross-source native-position ordering cannot collapse distinct source identities.
        assert!(clone.permits(&source.authority()));
        assert!(!other.permits(&source.authority()));
    }
}
