//! Persistent balanced piece-tree storage, coordinate indexes, and streaming traversal.

use super::*;
use std::num::NonZeroUsize;
use std::ops::ControlFlow;
use unicode_properties::{GeneralCategoryGroup, UnicodeGeneralCategory};

#[cfg(feature = "paged-shaping-frontier")]
pub(super) mod ascii_word;
mod original_span;
#[cfg(feature = "s2-table-window-prototype")]
pub(super) use original_span::ORIGINAL_SPAN_METADATA_STEPS;
pub(super) use original_span::{ORIGINAL_SPAN_PIECES, OriginalSpan, OriginalSpanBuilder};
mod composition_allocation;
pub(super) use composition_allocation::{
    CompositionAllocation, CompositionHeapCow, HeapAllocation,
};

/// Match the preserved editor's Unicode 17 `General_Category=L|N` word-run membership.
pub(super) fn is_word_character(character: char) -> bool {
    if character.is_ascii() {
        // ASCII L/N membership is exactly A-Z, a-z, and 0-9; avoid table searches on common Markdown bytes.
        return character.is_ascii_alphanumeric();
    }
    // `is_alphanumeric` includes Alphabetic combining marks; only exact letter/number categories match JS.
    matches!(
        character.general_category_group(),
        GeneralCategoryGroup::Letter | GeneralCategoryGroup::Number
    )
}

#[derive(Default)]
/// Session append segments retain stable IDs. Frozen readers share earlier
/// segments; only the bounded tail actually receiving bytes needs copy-on-write.
pub(super) struct MarkdownEditSegments {
    pub(super) segments: Vec<Arc<MarkdownEditSegment>>,
    pub(super) used_bytes: usize,
    // Advance only at transaction publication. A failed candidate file cannot
    // become a failure of the still-unchanged canonical text source.
    pub(super) published_segments: usize,
}

#[derive(Clone)]
/// Stable segment IDs identify byte ownership. Small inputs use the bounded
/// copy-on-write tail; large inputs use private immutable anonymous mappings.
pub(super) enum MarkdownEditSegment {
    Heap(Vec<u8>),
    Frozen(Arc<OriginalBacking>),
    // Retain the original slot's release facts only. Fixed atomics do not keep
    // an earlier candidate file or mapping alive.
    RetiredFrozen(Arc<crate::MarkdownOriginalBackingObserver>),
}

#[cfg(test)]
impl std::fmt::Debug for MarkdownEditSegment {
    /// Failure diagnostics expose storage kind and real length only, never reading
    /// an entire cell's text just to format a failed assertion.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Heap(bytes) => formatter.debug_tuple("Heap").field(&bytes.len()).finish(),
            Self::Frozen(backing) => formatter
                .debug_tuple("Frozen")
                .field(&backing.len())
                .finish(),
            Self::RetiredFrozen(observer) => formatter
                .debug_tuple("RetiredFrozen")
                .field(&observer.mapped_bytes())
                .finish(),
        }
    }
}

#[cfg(test)]
impl PartialEq for MarkdownEditSegment {
    /// Heap snapshots compare their actual bytes. Frozen-file rollback must retain
    /// original backing identity rather than create another file with equal bytes.
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Heap(left), Self::Heap(right)) => left == right,
            (Self::Frozen(left), Self::Frozen(right)) => Arc::ptr_eq(left, right),
            (Self::RetiredFrozen(left), Self::RetiredFrozen(right)) => Arc::ptr_eq(left, right),
            _ => false, // Failed transactions cannot silently replace heap storage with a file or copy files back to heap.
        }
    }
}

#[cfg(test)]
impl Eq for MarkdownEditSegment {}

impl MarkdownEditSegment {
    /// Returns the actual segment extent. Frozen storage exposes no full slice
    /// and cannot become a writable tail again.
    pub(super) fn len(&self) -> usize {
        match self {
            Self::Heap(bytes) => bytes.len(), // Actual tail bytes determine remaining append capacity.
            Self::Frozen(backing) => backing.len(), // Length grants no byte access; reads need a bounded lease.
            Self::RetiredFrozen(_) => 0, // A retired slot has no readable bytes; its ID retains final release facts.
        }
    }

    /// Hits and empty reads still check the mapping's first failure. Heap segments
    /// own no file resource requiring a health query.
    pub(super) fn health(&self) -> Result<(), MarkdownDocumentBufferError> {
        match self {
            Self::Heap(_) => Ok(()),
            Self::Frozen(backing) => backing.health().map_err(Into::into),
            Self::RetiredFrozen(observer) => observer.first_errno().map_or(Ok(()), |errno| {
                // A final frozen reader's delayed drop can fail. Retirement cannot
                // erase the first failure of an already published source.
                Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                    errno,
                ))
            }),
        }
    }

    /// Piece offsets belong to their original segment ID. Mapped reads preserve
    /// the 4-KiB lease limit and propagate actual release failures.
    pub(super) fn read(
        &self,
        range: Range<usize>,
        published: bool,
    ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError> {
        self.health()
            .map_err(|error| edit_read_error(error, published))?;
        match self {
            Self::Heap(bytes) => bytes
                .get(range)
                .map(PieceBytes::Edited)
                .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument),
            Self::Frozen(backing) => backing
                .lease(range)
                .map(|lease| PieceBytes::MappedEdit(lease, published))
                .map_err(|error| edit_read_error(error.into(), published)),
            Self::RetiredFrozen(_) => Err(MarkdownDocumentBufferError::RangeOutsideDocument),
        }
    }

    #[cfg(test)]
    /// Counts heap capacity only. Real backing observers separately verify
    /// frozen-mapping residency and final release.
    pub(super) fn capacity(&self) -> usize {
        match self {
            Self::Heap(bytes) => bytes.capacity(),
            Self::Frozen(_) | Self::RetiredFrozen(_) => 0,
        }
    }

    #[cfg(test)]
    /// Copy-on-write regressions inspect known heap segments only; they must not
    /// introduce a whole-segment reading API for frozen storage.
    pub(super) fn as_slice(&self) -> &[u8] {
        match self {
            Self::Heap(bytes) => bytes,
            Self::Frozen(_) | Self::RetiredFrozen(_) => panic!("封存编辑段只能通过有限lease读取"),
        }
    }
}

impl MarkdownEditSegments {
    /// Appends scalar-aligned UTF-8 leaves under the cumulative session budget.
    /// Tails shared by previous frozen readers remain immutable.
    pub(super) fn append(
        &mut self,
        text: &str,
        maximum_session_edit_bytes: usize,
    ) -> Result<Vec<Arc<MarkdownPieceTreeNode>>, MarkdownDocumentBufferError> {
        if text.is_empty() {
            // Empty input adds no bytes and allocates no segment, file, or empty leaf.
            return Ok(Vec::new());
        }
        if text.len() > EDIT_SEGMENT_BYTES {
            // Input larger than one tail segment is frozen rather than copied into
            // a document-sized heap inventory. The caller's string is not retained.
            let backing = self.freeze_input(text, maximum_session_edit_bytes)?;
            return self.append_frozen(backing, maximum_session_edit_bytes);
        }
        self.require_append_capacity(text.len(), maximum_session_edit_bytes)?;
        let mut leaves = Vec::with_capacity(text.len().div_ceil(ORIGINAL_PIECE_BYTES));
        let mut consumed = 0;
        while consumed < text.len() {
            // Consume one scalar-aligned bounded piece per turn, copying its bytes
            // into the actual writable tail only once.
            if self.segments.last().is_none_or(|segment| {
                // Only a heap tail with space is appendable; a frozen file never
                // regains write authority.
                !matches!(segment.as_ref(), MarkdownEditSegment::Heap(bytes) if bytes.len() < EDIT_SEGMENT_BYTES)
            }) {
                // Allocate a 64-KiB segment only when no writable tail remains, keeping
                // every existing immutable segment ID stable.
                self.segments
                    .push(Arc::new(MarkdownEditSegment::Heap(Vec::with_capacity(EDIT_SEGMENT_BYTES))));
            }
            let segment_index = self.segments.len() - 1;
            // Inspect tail length first, avoiding a copy of shared storage when its
            // remaining space cannot fit even the next complete scalar.
            let segment_start = self.segments[segment_index].len();
            let available = EDIT_SEGMENT_BYTES - segment_start;
            let requested = ORIGINAL_PIECE_BYTES
                .min(available)
                .min(text.len() - consumed);
            let mut chunk_end = consumed + requested;
            while !text.is_char_boundary(chunk_end) {
                // Retreat to a UTF-8 scalar boundary so the next leaf starts with a
                // complete scalar.
                chunk_end -= 1;
            }
            if chunk_end == consumed {
                // Abandon a tiny tail gap that cannot fit the next scalar. A fresh
                // segment guarantees forward progress without splitting UTF-8.
                self.segments
                    .push(Arc::new(MarkdownEditSegment::Heap(Vec::with_capacity(
                        EDIT_SEGMENT_BYTES,
                    ))));
                continue;
            }
            // Only the current tail may copy-on-write. Historical and frozen roots
            // retain their original segment bytes at the same stable IDs.
            let segment = Self::writable_segment(&mut self.segments[segment_index]);
            let chunk = &text.as_bytes()[consumed..chunk_end];
            segment.extend_from_slice(chunk);
            self.used_bytes = self
                .used_bytes
                .checked_add(chunk.len())
                .ok_or(MarkdownDocumentBufferError::EditBufferLimitExceeded)?;
            let piece = MarkdownPiece {
                source: MarkdownPieceSource::EditSegment(segment_index),
                byte_start: segment_start,
                byte_len: chunk.len(),
                metrics: MarkdownDocumentMetrics::from_text(
                    std::str::from_utf8(chunk).expect("显式str按完整UTF-8标量切片"),
                ),
            };
            leaves.push(Arc::new(MarkdownPieceTreeNode::Leaf(piece)));
            consumed = chunk_end;
        }
        Ok(leaves)
    }

    /// Freezes explicit long input through the same 4-KiB factory. Before installing
    /// it in the segment table, the transaction can take its actual release receipt.
    pub(super) fn freeze_input(
        &self,
        text: &str,
        maximum_session_edit_bytes: usize,
    ) -> Result<Arc<OriginalBacking>, MarkdownDocumentBufferError> {
        self.require_append_capacity(text.len(), maximum_session_edit_bytes)?;
        let mut output = crate::immutable_snapshot::CanonicalSnapshotOutput::new()
            .map_err(edit_snapshot_error)?;
        let mut consumed = 0;
        while consumed < text.len() {
            // Write complete bounded UTF-8 chunks only. A failed anonymous-file write
            // leaves both the segment table and cumulative budget unchanged.
            let mut end = (consumed + ORIGINAL_PIECE_BYTES).min(text.len());
            while !text.is_char_boundary(end) {
                end -= 1; // Retreat over at most one scalar; never write incomplete UTF-8 into the candidate.
            }
            output
                .write_all(&text.as_bytes()[consumed..end])
                .map_err(edit_snapshot_error)?;
            consumed = end;
        }
        let backing = output
            .freeze()
            .map_err(edit_snapshot_error)?
            .expect("非空显式输入已完整写入匿名文件");
        Ok(Arc::new(backing))
    }

    /// Checks cumulative admission before heap appends or anonymous-file effects.
    /// Deletion and saving never replenish the session append inventory.
    fn require_append_capacity(
        &self,
        bytes: usize,
        maximum_session_edit_bytes: usize,
    ) -> Result<(), MarkdownDocumentBufferError> {
        if self
            .used_bytes
            .checked_add(bytes)
            .is_none_or(|result| result > maximum_session_edit_bytes)
        {
            return Err(MarkdownDocumentBufferError::EditBufferLimitExceeded);
        }
        Ok(())
    }

    /// Installs a real frozen file in the same segment table. Publish its descriptor
    /// and cumulative byte count only after every bounded leaf read succeeds.
    pub(super) fn append_frozen(
        &mut self,
        backing: Arc<OriginalBacking>,
        maximum_session_edit_bytes: usize,
    ) -> Result<Vec<Arc<MarkdownPieceTreeNode>>, MarkdownDocumentBufferError> {
        backing
            .health()
            .map_err(|error| edit_read_error(error.into(), false))?;
        let used = self
            .used_bytes
            .checked_add(backing.len())
            .filter(|used| {
                // Share heap append's cumulative budget; deletion, undo, and save do
                // not replenish session storage admission.
                *used <= maximum_session_edit_bytes
            })
            .ok_or(MarkdownDocumentBufferError::EditBufferLimitExceeded)?;
        let index = self.segments.len();
        let mut leaves = Vec::with_capacity(backing.len().div_ceil(ORIGINAL_PIECE_BYTES));
        let mut offset = 0;
        while offset < backing.len() {
            // A fixed lease may end inside a scalar. Build the ordinary piece from
            // its complete UTF-8 prefix only.
            let lease = backing
                .lease(offset..(offset + ORIGINAL_PIECE_BYTES).min(backing.len()))
                .map_err(|error| edit_read_error(error.into(), false))?;
            let text = match std::str::from_utf8(&lease) {
                Ok(text) => Ok(text),
                Err(error) if error.error_len().is_none() => {
                    std::str::from_utf8(&lease[..error.valid_up_to()])
                        .map_err(|_| MarkdownDocumentBufferError::InvalidUtf8Document)
                }
                Err(_) => Err(MarkdownDocumentBufferError::InvalidUtf8Document),
            };
            let measured = text.map(|text| (text.len(), MarkdownDocumentMetrics::from_text(text)));
            lease
                .finish()
                .map_err(|error| edit_read_error(error.into(), false))?; // Real reclamation failure takes precedence; this candidate is not installed yet.
            let (length, metrics) = measured?;
            if length == 0 {
                // The output factory accepts bytes. Incomplete or invalid UTF-8 must
                // fail rather than create an empty leaf or retry forever.
                return Err(MarkdownDocumentBufferError::InvalidUtf8Document);
            }
            leaves.push(Arc::new(MarkdownPieceTreeNode::Leaf(MarkdownPiece {
                source: MarkdownPieceSource::EditSegment(index),
                byte_start: offset,
                byte_len: length,
                metrics,
            })));
            offset += length;
        }
        backing
            .health()
            .map_err(|error| edit_read_error(error.into(), false))?;
        self.segments
            .push(Arc::new(MarkdownEditSegment::Frozen(backing)));
        self.used_bytes = used;
        Ok(leaves)
    }

    /// Checks published frozen edit segments after the original mapping. Metadata
    /// reads cannot revive an edited source whose real storage has failed.
    pub(super) fn health(&self) -> Result<(), MarkdownDocumentBufferError> {
        for segment in &self.segments[..self.published_segments] {
            segment.health()?; // One real file failure stops reads; readable neighbors cannot replace it.
        }
        Ok(())
    }

    /// Pending files still belong to their preparing transaction. Their failure
    /// rejects this input without poisoning an unchanged canonical source.
    pub(super) fn pending_health(&self) -> Result<(), MarkdownDocumentBufferError> {
        for segment in &self.segments[self.published_segments..] {
            segment
                .health()
                .map_err(|error| edit_read_error(error, false))?;
        }
        Ok(())
    }

    /// Transfers segment source ownership at root publication without I/O,
    /// allocation, or fallible validation.
    pub(super) fn publish(&mut self) {
        self.published_segments = self.segments.len();
    }

    /// The segment table determines publication ownership itself. External numeric
    /// IDs cannot retroactively authorize a candidate as canonical source storage.
    pub(super) fn read(
        &self,
        index: usize,
        range: Range<usize>,
    ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError> {
        self.segments
            .get(index)
            .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?
            .read(range, index < self.published_segments)
    }

    #[cfg(test)]
    /// Measure retained edit capacity for storage regression tests, including unused segment tails.
    pub(super) fn allocated_bytes(&self) -> usize {
        self.segments.iter().map(|segment| segment.capacity()).sum()
    }

    /// `Arc::make_mut` copies a shared heap segment's live bytes on write. Restore
    /// fixed tail capacity so later small inputs do not repeatedly grow that copy.
    pub(super) fn writable_segment(segment: &mut Arc<MarkdownEditSegment>) -> &mut Vec<u8> {
        let MarkdownEditSegment::Heap(writable) = Arc::make_mut(segment) else {
            unreachable!("封存段不能进入可写堆尾路径");
        };
        if writable.capacity() < EDIT_SEGMENT_BYTES {
            // Capacity remains bounded to the original 64-KiB segment. Frozen readers
            // change neither cumulative append admission nor stable segment IDs.
            writable.reserve_exact(EDIT_SEGMENT_BYTES - writable.len());
        }
        writable
    }
}

/// Preserves real anonymous-output errno. Cancellation or extent failure cannot
/// be interpreted as a successful append.
pub(super) fn edit_snapshot_error(error: SnapshotError) -> MarkdownDocumentBufferError {
    match error {
        SnapshotError::Storage(error) => MarkdownDocumentBufferError::EditStorageUnavailable(
            error.raw_os_error().unwrap_or(libc::EIO),
        ),
        SnapshotError::SourceChanged => {
            MarkdownDocumentBufferError::EditStorageUnavailable(libc::EIO)
        }
        SnapshotError::Cancelled => MarkdownDocumentBufferError::PreparedTextInputStale,
    }
}

/// Only published mappings can revoke canonical-source health. During prepare,
/// the same system error belongs to this candidate's storage failure.
pub(super) fn edit_read_error(
    error: MarkdownDocumentBufferError,
    published: bool,
) -> MarkdownDocumentBufferError {
    match error {
        MarkdownDocumentBufferError::OriginalBackingUnavailable(errno) if !published => {
            MarkdownDocumentBufferError::EditStorageUnavailable(errno)
        }
        other => other, // Keep specific range/lease-limit failures; never substitute success or empty text.
    }
}

/// Trees request bounded leaves only. Return the original pin, complete required
/// reclamation, and verify source health before publishing visitor output.
pub(super) trait PieceByteSource {
    fn source_health(&self) -> Result<(), MarkdownDocumentBufferError>;

    fn piece_bytes(
        &self,
        piece: &MarkdownPiece,
    ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError>;

    /// Borrows an actual range within the original leaf. A word quantum cannot
    /// charge one byte while secretly leasing an entire leaf.
    #[cfg(feature = "paged-shaping-frontier")]
    fn piece_range_bytes(
        &self,
        piece: &MarkdownPiece,
        local: Range<usize>,
    ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError>;

    fn with_piece_bytes<R>(
        &self,
        piece: &MarkdownPiece,
        visit: impl FnOnce(&[u8]) -> Result<R, MarkdownDocumentBufferError>,
    ) -> Result<R, MarkdownDocumentBufferError> {
        let bytes = self.piece_bytes(piece)?;
        let result = visit(&bytes);
        // Return pins and complete required cleanup before propagating visitor
        // errors. A sentinel result must not hide an actual I/O failure.
        bytes.finish()?;
        result
    }
}

/// Heap edits borrow their original `Arc` bytes; mapped pieces carry real
/// reclamation leases. No raw slice may outlive its lease authority.
pub(super) enum PieceBytes<'a> {
    Original(crate::original_backing::OriginalReadLease<'a>),
    MappedEdit(crate::original_backing::OriginalReadLease<'a>, bool),
    Edited(&'a [u8]),
}

impl std::ops::Deref for PieceBytes<'_> {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Self::Original(lease) | Self::MappedEdit(lease, _) => lease,
            Self::Edited(bytes) => bytes,
        }
    }
}

impl PieceBytes<'_> {
    pub(super) fn finish(self) -> Result<(), MarkdownDocumentBufferError> {
        match self {
            Self::Original(lease) => lease.finish().map_err(Into::into),
            Self::MappedEdit(lease, published) => lease
                .finish()
                .map_err(|error| edit_read_error(error.into(), published)),
            // Heap edits own no resident file pages; the caller's `Arc` controls lifetime.
            Self::Edited(_) => Ok(()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Identifies the document-owned backing allocation for one immutable piece.
pub(super) enum MarkdownPieceSource {
    Original,
    EditSegment(usize),
}

#[derive(Clone, Debug)]
/// A scalar-aligned immutable byte view and its cached local metrics.
pub(super) struct MarkdownPiece {
    pub(super) source: MarkdownPieceSource,
    pub(super) byte_start: usize,
    pub(super) byte_len: usize,
    pub(super) metrics: MarkdownDocumentMetrics,
}

/// One byte records complete character-domain and ASCII-letter facts. Empty
/// input is the printable, letterless identity for aggregate joins.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum PrintableAsciiClass {
    NonPrintable,
    #[default]
    WithoutLetters,
    WithLetters,
}
impl PrintableAsciiClass {
    pub(super) fn is_printable(self) -> bool {
        self != Self::NonPrintable
    }
    pub(super) fn has_letters(self) -> Option<bool> {
        match self {
            Self::NonPrintable => None, // Mixed Unicode/controls grant no script fast path, even if letters occurred.
            Self::WithoutLetters => Some(false),
            Self::WithLetters => Some(true),
        }
    }
    fn scalar(scalar: char) -> Self {
        if !(' '..='~').contains(&scalar) {
            // Any control or non-ASCII scalar revokes printable-ASCII authority for
            // the complete aggregate.
            Self::NonPrintable
        } else if scalar.is_ascii_alphabetic() {
            // Only an actual ASCII letter grants the Latin fact; digits and
            // punctuation cannot substitute for it.
            Self::WithLetters
        } else {
            // Printable digits, spaces, and punctuation remain Common candidates
            // until another subtree contributes an actual letter.
            Self::WithoutLetters
        }
    }
    fn joined(self, other: Self) -> Self {
        match (self, other) {
            // Nonprintable facts absorb the join; letters on the other side cannot
            // restore fast-path authority.
            (Self::NonPrintable, _) | (_, Self::NonPrintable) => Self::NonPrintable,
            // Once both sides are printable, either side's actual letter grants
            // the joined Latin fact.
            (Self::WithLetters, _) | (_, Self::WithLetters) => Self::WithLetters,
            // Empty text and printable digits/punctuation form the letterless identity.
            _ => Self::WithoutLetters,
        } // Propagate negative domain facts; OR letter presence only inside a printable domain.
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Joinable text aggregates preserving cross-piece CRLF, word boundaries,
/// and exact character-domain facts.
pub(super) struct MarkdownDocumentMetrics {
    pub(super) byte_len: usize,
    pub(super) utf16_len: usize,
    pub(super) character_count: usize,
    pub(super) word_count: usize,
    pub(super) starts_in_word: bool,
    pub(super) ends_in_word: bool,
    pub(super) line_breaks: usize,
    // Text-checking terminators are counted independently: CRLF has two units;
    // paragraph separator counts, while NEL and line separator do not.
    pub(super) text_checking_terminators: usize,
    // UTF-16 offset-plus-one encodes first/last terminators. `None` needs no
    // extra tag, and no per-leaf character index is allocated.
    pub(super) first_text_checking_terminator: Option<NonZeroUsize>,
    pub(super) last_text_checking_terminator: Option<NonZeroUsize>,
    pub(super) starts_with_line_feed: bool,
    pub(super) ends_with_carriage_return: bool,
    pub(super) contains_carriage_return: bool,
    pub(super) contains_crlf: bool,
    /// A raw '<' anywhere can introduce an inherited HTML block outside a local cue envelope.
    pub(super) contains_ascii_less_than: bool,
    /// Reuses one byte for nonprintable, printable-letterless, and printable-with-
    /// letters states without growing every tree node.
    pub(super) all_printable_ascii: PrintableAsciiClass,
    // Records actual Unicode properties only, not bidi/run support policy or
    // authority to read source bytes.
    pub(super) unicode_properties: MarkdownUnicodeProperties,
}

impl Default for MarkdownDocumentMetrics {
    fn default() -> Self {
        // Empty input has no risky character and vacuously satisfies the printable
        // ASCII domain; a default false flag would break aggregate identity.
        Self {
            byte_len: 0,
            utf16_len: 0,
            character_count: 0,
            word_count: 0,
            starts_in_word: false,
            ends_in_word: false,
            line_breaks: 0,
            text_checking_terminators: 0,
            first_text_checking_terminator: None,
            last_text_checking_terminator: None,
            starts_with_line_feed: false,
            ends_with_carriage_return: false,
            contains_carriage_return: false,
            contains_crlf: false,
            contains_ascii_less_than: false,
            all_printable_ascii: PrintableAsciiClass::WithoutLetters,
            unicode_properties: MarkdownUnicodeProperties::default(),
        }
    }
}

impl MarkdownDocumentMetrics {
    /// Compute associative document aggregates for one scalar-aligned leaf without materializing auxiliary indexes.
    pub(super) fn from_text(text: &str) -> Self {
        let bytes = text.as_bytes();
        let mut metrics = Self {
            byte_len: bytes.len(),
            starts_with_line_feed: bytes.first() == Some(&b'\n'),
            ends_with_carriage_return: bytes.last() == Some(&b'\r'),
            starts_in_word: text.chars().next().is_some_and(|character| {
                // A piece beginning with an L/N scalar may continue a word ending in its left neighbor.
                is_word_character(character)
            }),
            ..Self::default()
        };
        let mut previous_was_carriage_return = false;
        for character in text.chars() {
            // Collect properties in the same bounded leaf pass, so later layout does
            // not rescan an unread suffix merely to establish its domain.
            metrics.unicode_properties.observe(character);
            // Visit each Unicode scalar once; only the previous CR and word state cross iterations.
            // Normalize a CRLF pair to one visible character, including across piece joins below.
            if character != '\n' || !previous_was_carriage_return {
                // A CRLF continuation shares its visible-character count with the preceding CR.
                metrics.character_count += 1;
            }
            // Cache the exact baseline regex runs without allocating tokens or applying locale segmentation.
            let in_word = is_word_character(character);
            if in_word && !metrics.ends_in_word {
                // Punctuation, marks, emoji, controls, and line endings separate successive L/N runs.
                metrics.word_count += 1;
            }
            metrics.ends_in_word = in_word;
            // Reuse this scalar pass. C0, DEL, and every non-ASCII scalar revoke the
            // domain without an additional text scan.
            metrics.all_printable_ascii = metrics
                .all_printable_ascii
                .joined(PrintableAsciiClass::scalar(character));
            if matches!(character, '\r' | '\n' | '\u{2029}') {
                // Capture paragraph endpoints in this existing scalar pass instead of
                // scanning original text again for each later query.
                // Record before advancing length, so an interior-surrogate query cannot
                // redefine the terminator's own UTF-16 position.
                let offset = NonZeroUsize::new(metrics.utf16_len + 1)
                    .expect("a UTF-16 terminator offset plus one is nonzero");
                metrics.text_checking_terminators += 1;
                if metrics.first_text_checking_terminator.is_none() {
                    // Capture the first terminator once; later ones update only the final
                    // endpoint and raw terminator-unit count.
                    metrics.first_text_checking_terminator = Some(offset);
                }
                metrics.last_text_checking_terminator = Some(offset);
            }
            metrics.utf16_len += character.len_utf16();
            // Maintain conservative syntax risk in the existing aggregate pass, including edited pieces.
            metrics.contains_ascii_less_than |= character == '<';
            match character {
                // A CR starts a line break immediately; a later LF may merge with it.
                '\r' => {
                    metrics.line_breaks += 1;
                    metrics.contains_carriage_return = true;
                    previous_was_carriage_return = true;
                }
                // The LF completes the existing CRLF break and records the line-ending form.
                '\n' if previous_was_carriage_return => {
                    metrics.contains_crlf = true;
                    previous_was_carriage_return = false;
                }
                // A standalone LF starts its own line break.
                '\n' => {
                    metrics.line_breaks += 1;
                    previous_was_carriage_return = false;
                }
                // Ordinary text ends the CRLF lookbehind state.
                _ => previous_was_carriage_return = false,
            }
        }
        metrics
    }

    /// Merge adjacent subtree aggregates, reconciling CRLF and word runs split at the join.
    pub(super) fn joined(left: Self, right: Self) -> Self {
        if left.byte_len == 0 {
            // An empty left side contributes no boundary state and acts as the identity.
            return right;
        }
        if right.byte_len == 0 {
            // An empty right side likewise preserves all left aggregate state.
            return left;
        }
        let boundary_is_crlf = left.ends_with_carriage_return && right.starts_with_line_feed;
        Self {
            byte_len: left.byte_len + right.byte_len,
            utf16_len: left.utf16_len + right.utf16_len,
            character_count: left.character_count + right.character_count
                - usize::from(boundary_is_crlf),
            word_count: left.word_count + right.word_count
                - usize::from(left.ends_in_word && right.starts_in_word),
            starts_in_word: left.starts_in_word,
            ends_in_word: right.ends_in_word,
            // Both sides must prove the exact domain. Equal byte/UTF-16 lengths
            // cannot authorize the same fast path for unread text.
            all_printable_ascii: left.all_printable_ascii.joined(right.all_printable_ascii),
            unicode_properties: left.unicode_properties.joined(right.unicode_properties),
            line_breaks: left.line_breaks + right.line_breaks - usize::from(boundary_is_crlf),
            // This query distinguishes CR from a following LF; it does not inherit
            // physical line counting's cross-leaf CRLF merge.
            text_checking_terminators: left.text_checking_terminators
                + right.text_checking_terminators,
            first_text_checking_terminator: left.first_text_checking_terminator.or_else(|| {
                // Shift the right endpoint by left UTF-16 length only, preserving its
                // offset-plus-one encoding in the joined coordinate space.
                right.first_text_checking_terminator.map(|offset| {
                    NonZeroUsize::new(left.utf16_len + offset.get())
                        .expect("a shifted terminator offset remains nonzero")
                })
            }),
            last_text_checking_terminator: right
                .last_text_checking_terminator
                .map(|offset| {
                    // A right-side terminator is necessarily the joined final endpoint;
                    // otherwise retain the left endpoint in its original coordinates.
                    NonZeroUsize::new(left.utf16_len + offset.get())
                        .expect("a shifted terminator offset remains nonzero")
                })
                .or(left.last_text_checking_terminator),
            starts_with_line_feed: left.starts_with_line_feed,
            ends_with_carriage_return: right.ends_with_carriage_return,
            contains_carriage_return: left.contains_carriage_return
                || right.contains_carriage_return,
            contains_crlf: left.contains_crlf || right.contains_crlf || boundary_is_crlf,
            contains_ascii_less_than: left.contains_ascii_less_than
                || right.contains_ascii_less_than,
        }
    }
}

/// An absent root denotes empty text without a synthetic sentinel allocation.
pub(super) type PieceTreeLink = Option<Arc<MarkdownPieceTreeNode>>;

#[derive(Clone, Debug)]
/// Persistent AVL node; Arc ownership lets history share unchanged immutable subtrees.
pub(super) enum MarkdownPieceTreeNode {
    Leaf(MarkdownPiece),
    /// Grouped physical leaves share a small directory; readers still borrow
    /// only one logical piece up to 4 KiB at a time.
    OriginalSpan(OriginalSpan),
    Branch {
        left: Arc<MarkdownPieceTreeNode>,
        right: Arc<MarkdownPieceTreeNode>,
        metrics: MarkdownDocumentMetrics,
        height: u16,
    },
}

impl MarkdownPieceTreeNode {
    /// Read cached subtree aggregates without scanning document text.
    pub(super) fn metrics(&self) -> MarkdownDocumentMetrics {
        match self {
            // A leaf owns the metrics of its immutable byte slice.
            Self::Leaf(piece) => piece.metrics,
            // Grouped leaves retain joined metrics; whole-tree aggregates require
            // no expansion of their directory.
            Self::OriginalSpan(span) => span.metrics,
            // A branch stores the already-reconciled aggregate of its children.
            Self::Branch { metrics, .. } => *metrics,
        }
    }

    /// Read the cached AVL height used by path-copying joins and rotations.
    pub(super) fn height(&self) -> u16 {
        match self {
            // A leaf is the nonempty tree height base case.
            Self::Leaf(_) => 1,
            // The small directory is one physical leaf; internal 4-KiB records add
            // no AVL levels.
            Self::OriginalSpan(_) => 1,
            // A branch carries its height so balancing never walks the whole subtree.
            Self::Branch { height, .. } => *height,
        }
    }
}

pub(super) struct OriginalTreeBuild {
    pub(super) root: PieceTreeLink,
    pub(super) revision: MarkdownDocumentRevision,
    #[cfg(any(test, feature = "native-markdown-candidate"))]
    pub(super) checkpoints: MarkdownBlockCheckpointIndex,
}

/// Initial open leases the same immutable mapping in 4-KiB chunks. A partial
/// trailing UTF-8 scalar remains in backing for the following chunk.
/// Each bounded chunk completes real lease cleanup after hashing, metrics, and
/// checkpoint consumption, without a whole-document string or text copy.
pub(super) fn build_original_backing_tree(
    backing: Option<&crate::original_backing::OriginalBacking>,
    range: Range<usize>,
) -> Result<OriginalTreeBuild, MarkdownDocumentBufferError> {
    let mut leaves = Vec::with_capacity(
        range
            .len()
            .div_ceil(ORIGINAL_PIECE_BYTES * ORIGINAL_SPAN_PIECES),
    );
    let mut spans = OriginalSpanBuilder::default();
    let mut hasher = Sha256::new();
    #[cfg(any(test, feature = "native-markdown-candidate"))]
    let mut scanner = InitialBlockScanner::new();
    let mut byte_start = range.start;
    while byte_start < range.end {
        let backing = backing.ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
        let end = byte_start
            .saturating_add(ORIGINAL_PIECE_BYTES)
            .min(range.end);
        let consumed = backing.with_bytes(byte_start..end, |bytes| {
            let text = match std::str::from_utf8(bytes) {
                Ok(text) => text,
                // Only an incomplete scalar at a nonfinal chunk boundary can continue.
                // Actual EOF or an invalid byte sequence must fail immediately.
                Err(error)
                    if error.error_len().is_none()
                        && end < range.end
                        && error.valid_up_to() > 0 =>
                {
                    std::str::from_utf8(&bytes[..error.valid_up_to()])
                        .expect("validated UTF-8 prefix")
                }
                Err(_) => return Err(MarkdownDocumentBufferError::InvalidUtf8Document),
            };
            hasher.update(text.as_bytes());
            #[cfg(any(test, feature = "native-markdown-candidate"))]
            scanner.push(text);
            spans.push(
                MarkdownPiece {
                    source: MarkdownPieceSource::Original,
                    byte_start: byte_start - range.start,
                    byte_len: text.len(),
                    metrics: MarkdownDocumentMetrics::from_text(text),
                },
                &mut leaves,
            )?;
            Ok(text.len())
        })??;
        byte_start += consumed;
    }
    spans.flush(&mut leaves)?;
    Ok(OriginalTreeBuild {
        root: build_balanced_piece_tree(leaves),
        revision: MarkdownDocumentRevision(hasher.finalize().into()),
        #[cfg(any(test, feature = "native-markdown-candidate"))]
        checkpoints: scanner.finish(),
    })
}

#[cfg(test)]
/// Index the mapped original text in bounded UTF-8 leaves while hashing the initial revision.
pub(super) fn build_original_piece_tree(text: &str) -> (PieceTreeLink, MarkdownDocumentRevision) {
    let mut leaves = Vec::with_capacity(
        text.len()
            .div_ceil(ORIGINAL_PIECE_BYTES * ORIGINAL_SPAN_PIECES),
    );
    let mut spans = OriginalSpanBuilder::default();
    let mut hasher = Sha256::new();
    let mut byte_start = 0;
    while byte_start < text.len() {
        // Build each leaf and hash the same byte sequence in document order.
        let mut byte_end = (byte_start + ORIGINAL_PIECE_BYTES).min(text.len());
        while !text.is_char_boundary(byte_end) {
            // Keep leaf endpoints on scalar boundaries so later slices can be decoded independently.
            byte_end -= 1;
        }
        let slice = &text[byte_start..byte_end];
        hasher.update(slice.as_bytes());
        spans
            .push(
                MarkdownPiece {
                    source: MarkdownPieceSource::Original,
                    byte_start,
                    byte_len: slice.len(),
                    metrics: MarkdownDocumentMetrics::from_text(slice),
                },
                &mut leaves,
            )
            .expect("有限已验证原始片必须可合组");
        byte_start = byte_end;
    }
    if text.is_empty() {
        // The empty document still receives the canonical SHA-256 digest for zero bytes.
        hasher.update([]);
    }
    let revision = MarkdownDocumentRevision(hasher.finalize().into());
    spans
        .flush(&mut leaves)
        .expect("有限原始Span目录必须可封闭");
    (build_balanced_piece_tree(leaves), revision)
}

/// Turn ordered leaves into a balanced persistent root, using None for empty text.
pub(super) fn build_balanced_piece_tree(nodes: Vec<Arc<MarkdownPieceTreeNode>>) -> PieceTreeLink {
    if nodes.is_empty() {
        // An empty leaf collection has no root and allocates no sentinel node.
        return None;
    }
    Some(build_balanced_piece_tree_slice(&nodes))
}

/// Recursively bisect ordered leaves to establish a balanced initial tree.
pub(super) fn build_balanced_piece_tree_slice(
    nodes: &[Arc<MarkdownPieceTreeNode>],
) -> Arc<MarkdownPieceTreeNode> {
    if nodes.len() == 1 {
        // Reuse a singleton leaf by Arc rather than copying its text or metadata.
        return Arc::clone(&nodes[0]);
    }
    let middle = nodes.len() / 2;
    make_piece_tree_branch(
        build_balanced_piece_tree_slice(&nodes[..middle]),
        build_balanced_piece_tree_slice(&nodes[middle..]),
    )
}

/// Create one immutable branch with recomputed metrics and height from its owned children.
pub(super) fn make_piece_tree_branch(
    left: Arc<MarkdownPieceTreeNode>,
    right: Arc<MarkdownPieceTreeNode>,
) -> Arc<MarkdownPieceTreeNode> {
    Arc::new(MarkdownPieceTreeNode::Branch {
        metrics: MarkdownDocumentMetrics::joined(left.metrics(), right.metrics()),
        height: left.height().max(right.height()) + 1,
        left,
        right,
    })
}

/// Concatenate optional trees while preserving byte order and sharing unchanged roots.
pub(super) fn join_piece_trees(left: PieceTreeLink, right: PieceTreeLink) -> PieceTreeLink {
    match (left, right) {
        // An absent prefix leaves the suffix root unchanged.
        (None, right) => right,
        // An absent suffix leaves the prefix root unchanged.
        (left, None) => left,
        // Two nonempty sides require a height-aware persistent join.
        (Some(left), Some(right)) => Some(join_piece_tree_nodes(left, right)),
    }
}

/// Join nonempty AVL trees by descending only the taller spine and rebuilding that path.
pub(super) fn join_piece_tree_nodes(
    left: Arc<MarkdownPieceTreeNode>,
    right: Arc<MarkdownPieceTreeNode>,
) -> Arc<MarkdownPieceTreeNode> {
    let left_height = left.height();
    let right_height = right.height();
    if left_height > right_height + 1 {
        // Descend the left tree's right spine so the joined subtree remains height compatible.
        let MarkdownPieceTreeNode::Branch {
            left: left_left,
            right: left_right,
            ..
        } = left.as_ref()
        else {
            // A leaf has height one, so the validated height imbalance requires a branch here.
            unreachable!("an imbalanced AVL child cannot be a leaf");
        };
        return rebalance_piece_tree(
            Arc::clone(left_left),
            join_piece_tree_nodes(Arc::clone(left_right), right),
        );
    }
    if right_height > left_height + 1 {
        // Descend the right tree's left spine for the symmetric height imbalance.
        let MarkdownPieceTreeNode::Branch {
            left: right_left,
            right: right_right,
            ..
        } = right.as_ref()
        else {
            // A leaf has height one, so the validated height imbalance requires a branch here.
            unreachable!("an imbalanced AVL child cannot be a leaf");
        };
        return rebalance_piece_tree(
            join_piece_tree_nodes(left, Arc::clone(right_left)),
            Arc::clone(right_right),
        );
    }
    make_piece_tree_branch(left, right)
}

/// Restore the AVL height invariant with order-preserving single or double rotations.
pub(super) fn rebalance_piece_tree(
    left: Arc<MarkdownPieceTreeNode>,
    right: Arc<MarkdownPieceTreeNode>,
) -> Arc<MarkdownPieceTreeNode> {
    if left.height() > right.height() + 1 {
        // A left-heavy pair must rotate before becoming a reusable branch.
        let MarkdownPieceTreeNode::Branch {
            left: left_left,
            right: left_right,
            ..
        } = left.as_ref()
        else {
            // A leaf has height one, so the validated height imbalance requires a branch here.
            unreachable!("an imbalanced AVL child cannot be a leaf");
        };
        if left_left.height() >= left_right.height() {
            // An outer-heavy left child needs one right rotation; all unaffected subtrees remain shared.
            return make_piece_tree_branch(
                Arc::clone(left_left),
                make_piece_tree_branch(Arc::clone(left_right), right),
            );
        }
        let MarkdownPieceTreeNode::Branch {
            left: middle_left,
            right: middle_right,
            ..
        } = left_right.as_ref()
        else {
            // The inner child is taller than its leaf sibling, which requires another branch.
            unreachable!("a double rotation requires an inner branch");
        };
        return make_piece_tree_branch(
            make_piece_tree_branch(Arc::clone(left_left), Arc::clone(middle_left)),
            make_piece_tree_branch(Arc::clone(middle_right), right),
        );
    }
    if right.height() > left.height() + 1 {
        // A right-heavy pair uses the symmetric rotation rules.
        let MarkdownPieceTreeNode::Branch {
            left: right_left,
            right: right_right,
            ..
        } = right.as_ref()
        else {
            // A leaf has height one, so the validated height imbalance requires a branch here.
            unreachable!("an imbalanced AVL child cannot be a leaf");
        };
        if right_right.height() >= right_left.height() {
            // An outer-heavy right child needs one left rotation rather than a double rotation.
            return make_piece_tree_branch(
                make_piece_tree_branch(left, Arc::clone(right_left)),
                Arc::clone(right_right),
            );
        }
        let MarkdownPieceTreeNode::Branch {
            left: middle_left,
            right: middle_right,
            ..
        } = right_left.as_ref()
        else {
            // The inner child is taller than its leaf sibling, which requires another branch.
            unreachable!("a double rotation requires an inner branch");
        };
        return make_piece_tree_branch(
            make_piece_tree_branch(left, Arc::clone(middle_left)),
            make_piece_tree_branch(Arc::clone(middle_right), Arc::clone(right_right)),
        );
    }
    make_piece_tree_branch(left, right)
}

/// Split at a scalar-aligned byte position, sharing untouched subtrees for undo history.
pub(super) fn split_piece_tree(
    document: &impl PieceByteSource,
    root: PieceTreeLink,
    byte_offset: usize,
) -> Result<(PieceTreeLink, PieceTreeLink), MarkdownDocumentBufferError> {
    document.source_health()?;
    let Some(root) = root else {
        // Empty storage has only its zero boundary; reject any positive requested extent below.
        return if byte_offset == 0 {
            // Only offset zero exists in an empty document.
            Ok((None, None))
        } else {
            // A nonzero position cannot be represented by an absent root.
            Err(MarkdownDocumentBufferError::RangeOutsideDocument)
        };
    };
    if byte_offset == 0 {
        // A zero split transfers the complete existing tree to the suffix.
        return Ok((None, Some(root)));
    }
    if byte_offset == root.metrics().byte_len {
        // An end split transfers the complete existing tree to the prefix.
        return Ok((Some(root), None));
    }
    if byte_offset > root.metrics().byte_len {
        // Offsets beyond the aggregate byte length are rejected before slicing.
        return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
    }

    match root.as_ref() {
        // Only the intersected leaf is divided; its bytes continue to borrow their original storage.
        MarkdownPieceTreeNode::Leaf(piece) => document.with_piece_bytes(piece, |bytes| {
            if !std::str::from_utf8(bytes)
                .expect("piece bytes remain valid UTF-8")
                .is_char_boundary(byte_offset)
            {
                // Reject a split inside a UTF-8 scalar rather than producing invalid leaf slices.
                return Err(MarkdownDocumentBufferError::NotUnicodeBoundary);
            }
            let left_piece = markdown_piece_subrange(piece, bytes, 0..byte_offset);
            let right_piece = markdown_piece_subrange(piece, bytes, byte_offset..bytes.len());
            Ok((
                Some(Arc::new(MarkdownPieceTreeNode::Leaf(left_piece))),
                Some(Arc::new(MarkdownPieceTreeNode::Leaf(right_piece))),
            ))
        }),
        MarkdownPieceTreeNode::OriginalSpan(span) => {
            // Locate the real logical piece first. Complete directory prefixes
            // require no source leases and do not enlarge the bounded leaf.
            let hit = span.locate_byte(byte_offset, true)?;
            if hit.local == hit.piece.byte_len {
                // A piece-boundary split only narrows directory ranges. It neither
                // reads text nor turns the full span into one oversized leaf.
                return Ok((
                    span.subrange(span.record_range().start..hit.ordinal + 1)?,
                    span.subrange(hit.ordinal + 1..span.record_range().end)?,
                ));
            }
            let before = span.subrange(span.record_range().start..hit.ordinal)?;
            let after = span.subrange(hit.ordinal + 1..span.record_range().end)?;
            // Split only one actual piece up to 4 KiB through the Unicode-aware
            // path; complete pieces on either side keep their shared directory.
            let (left, right) = split_piece_tree(
                document,
                Some(Arc::new(MarkdownPieceTreeNode::Leaf(hit.piece))),
                hit.local,
            )?;
            Ok((
                join_piece_trees(before, left),
                join_piece_trees(right, after),
            ))
        }
        // Use subtree byte aggregates to split only the side containing the requested boundary.
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            let left_bytes = left.metrics().byte_len;
            if byte_offset < left_bytes {
                // A boundary inside the left child retains the right child as part of the suffix.
                let (prefix, left_suffix) =
                    split_piece_tree(document, Some(Arc::clone(left)), byte_offset)?;
                Ok((
                    prefix,
                    join_piece_trees(left_suffix, Some(Arc::clone(right))),
                ))
            } else if byte_offset == left_bytes {
                // A boundary between children can reuse both roots without allocating leaf fragments.
                Ok((Some(Arc::clone(left)), Some(Arc::clone(right))))
            } else {
                // A boundary inside the right child retains the left child as part of the prefix.
                let (right_prefix, suffix) =
                    split_piece_tree(document, Some(Arc::clone(right)), byte_offset - left_bytes)?;
                Ok((
                    join_piece_trees(Some(Arc::clone(left)), right_prefix),
                    suffix,
                ))
            }
        }
    }
}

/// Create a metadata-only slice of a validated UTF-8 piece and recompute its local aggregates.
pub(super) fn markdown_piece_subrange(
    piece: &MarkdownPiece,
    bytes: &[u8],
    range: Range<usize>,
) -> MarkdownPiece {
    let text = std::str::from_utf8(&bytes[range.clone()]).expect("piece subrange is UTF-8");
    MarkdownPiece {
        source: piece.source,
        byte_start: piece.byte_start + range.start,
        byte_len: range.len(),
        metrics: MarkdownDocumentMetrics::from_text(text),
    }
}

/// Translates UTF-16 scalar boundaries using cached subtree lengths and at most
/// one leaf scan. Byte providers hold no document-mutation authority.
pub(super) fn byte_offset_for_utf16_in_tree(
    document: &impl PieceByteSource,
    root: Option<&MarkdownPieceTreeNode>,
    utf16_offset: usize,
    accumulated_bytes: usize,
) -> Result<usize, MarkdownDocumentBufferError> {
    document.source_health()?;
    if utf16_offset == 0 {
        // The subtree start is already a verified UTF-8 boundary. Check health,
        // but do not lease its first leaf merely to query that fixed position:
        // revalidating 4 KiB would needlessly disturb the bounded page directory
        // currently serving a distant viewport.
        return Ok(accumulated_bytes);
    }
    let Some(root) = root else {
        // The empty tree's only boundary was handled above. No positive position
        // belongs to an absent subtree.
        return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
    };
    match root {
        // Decode the selected leaf to distinguish scalar boundaries from surrogate interiors.
        MarkdownPieceTreeNode::Leaf(piece) => document.with_piece_bytes(piece, |bytes| {
            let text = std::str::from_utf8(bytes).expect("piece bytes remain valid UTF-8");
            let mut traversed_utf16 = 0;
            for (byte_index, character) in text.char_indices() {
                // Advance by Unicode scalar while retaining the UTF-16 length consumed before it.
                if traversed_utf16 == utf16_offset {
                    // A completed scalar prefix identifies an exact byte boundary.
                    return Ok(accumulated_bytes + byte_index);
                }
                let next = traversed_utf16 + character.len_utf16();
                if utf16_offset < next {
                    // The requested unit lies inside a surrogate pair and cannot define a text edit boundary.
                    return Err(MarkdownDocumentBufferError::NotUnicodeBoundary);
                }
                traversed_utf16 = next;
            }
            if traversed_utf16 == utf16_offset {
                // The position immediately after the final scalar is a valid leaf boundary.
                Ok(accumulated_bytes + text.len())
            } else {
                // Any remaining requested units lie beyond this leaf.
                Err(MarkdownDocumentBufferError::RangeOutsideDocument)
            }
        }),
        MarkdownPieceTreeNode::OriginalSpan(span) => {
            // Directory metadata selects one logical piece and cached prefix; scalar
            // validation still follows the same bounded decoding path.
            let hit = span.locate_utf16(utf16_offset, true)?;
            let leaf = MarkdownPieceTreeNode::Leaf(hit.piece);
            byte_offset_for_utf16_in_tree(
                document,
                Some(&leaf),
                hit.local,
                accumulated_bytes + hit.prefix.byte_len,
            )
        }
        // Descend using cached UTF-16 lengths while carrying the corresponding byte prefix.
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            if utf16_offset <= left.metrics().utf16_len {
                // The target boundary belongs to the left subtree, including its end position.
                byte_offset_for_utf16_in_tree(
                    document,
                    Some(left.as_ref()),
                    utf16_offset,
                    accumulated_bytes,
                )
            } else {
                // Subtract the left UTF-16 prefix and add its bytes before entering the right subtree.
                byte_offset_for_utf16_in_tree(
                    document,
                    Some(right.as_ref()),
                    utf16_offset - left.metrics().utf16_len,
                    accumulated_bytes + left.metrics().byte_len,
                )
            }
        }
    }
}

/// Translate a small standalone string boundary with the same surrogate rejection as tree coordinates.
pub(super) fn byte_offset_for_utf16_text(
    text: &str,
    utf16_offset: usize,
) -> Result<usize, MarkdownDocumentBufferError> {
    let mut traversed_utf16 = 0;
    for (byte_index, character) in text.char_indices() {
        // Inspect scalar boundaries in order without allocating a UTF-16 copy.
        if traversed_utf16 == utf16_offset {
            // The consumed unit count identifies an exact byte boundary.
            return Ok(byte_index);
        }
        let next = traversed_utf16 + character.len_utf16();
        if utf16_offset < next {
            // A partial surrogate pair cannot be used as a replacement endpoint.
            return Err(MarkdownDocumentBufferError::NotUnicodeBoundary);
        }
        traversed_utf16 = next;
    }
    if traversed_utf16 == utf16_offset {
        // The end-of-string boundary is valid after all scalars are consumed.
        Ok(text.len())
    } else {
        // An offset beyond the encoded length is outside the supplied text.
        Err(MarkdownDocumentBufferError::RangeOutsideDocument)
    }
}

/// Translates scalar-aligned byte positions. Live documents and frozen readers
/// share exactly the same invalid-boundary rejection rules.
pub(super) fn utf16_offset_for_byte_in_tree(
    document: &impl PieceByteSource,
    root: Option<&MarkdownPieceTreeNode>,
    byte_offset: usize,
    accumulated_utf16: usize,
) -> Result<usize, MarkdownDocumentBufferError> {
    document.source_health()?;
    let Some(root) = root else {
        // Empty storage has only its zero boundary; reject any positive requested extent below.
        return if byte_offset == 0 {
            // The empty subtree contributes no UTF-16 units at its only boundary.
            Ok(accumulated_utf16)
        } else {
            // A positive byte offset requires nonempty storage.
            Err(MarkdownDocumentBufferError::RangeOutsideDocument)
        };
    };
    match root {
        // Decode only the selected leaf to count the UTF-16 prefix.
        MarkdownPieceTreeNode::Leaf(piece) => document.with_piece_bytes(piece, |bytes| {
            let text = std::str::from_utf8(bytes).expect("piece bytes remain valid UTF-8");
            if !text.is_char_boundary(byte_offset) {
                // A byte inside a multibyte scalar has no valid UTF-16 edit boundary.
                return Err(MarkdownDocumentBufferError::NotUnicodeBoundary);
            }
            Ok(accumulated_utf16 + text[..byte_offset].encode_utf16().count())
        }),
        MarkdownPieceTreeNode::OriginalSpan(span) => {
            // Decode only the selected piece for byte-to-UTF-16 conversion; complete
            // directory prefixes require no source lease.
            let hit = span.locate_byte(byte_offset, true)?;
            let leaf = MarkdownPieceTreeNode::Leaf(hit.piece);
            utf16_offset_for_byte_in_tree(
                document,
                Some(&leaf),
                hit.local,
                accumulated_utf16 + hit.prefix.utf16_len,
            )
        }
        // Branch aggregates select a single descent and preserve the accumulated UTF-16 prefix.
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            if byte_offset <= left.metrics().byte_len {
                // The boundary lies in the left subtree, including the shared child boundary.
                utf16_offset_for_byte_in_tree(
                    document,
                    Some(left.as_ref()),
                    byte_offset,
                    accumulated_utf16,
                )
            } else {
                // The right-side coordinate excludes the left bytes and includes its UTF-16 length.
                utf16_offset_for_byte_in_tree(
                    document,
                    Some(right.as_ref()),
                    byte_offset - left.metrics().byte_len,
                    accumulated_utf16 + left.metrics().utf16_len,
                )
            }
        }
    }
}

/// Reads one UTF-16 unit. NSString may read either surrogate half and does not
/// require an edit-valid scalar boundary.
pub(super) fn utf16_code_unit_in_tree(
    document: &impl PieceByteSource,
    root: Option<&MarkdownPieceTreeNode>,
    utf16_offset: usize,
) -> Result<u16, MarkdownDocumentBufferError> {
    document.source_health()?;
    let root = root.ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
    match root {
        // Only the selected leaf needs scalar decoding; nth may intentionally return a surrogate half.
        MarkdownPieceTreeNode::Leaf(piece) => document.with_piece_bytes(piece, |bytes| {
            let text = std::str::from_utf8(bytes).expect("piece bytes remain valid UTF-8");
            text.encode_utf16()
                .nth(utf16_offset)
                .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)
        }),
        MarkdownPieceTreeNode::OriginalSpan(span) => {
            // The requested unit must lie in an actual piece. A piece end or EOF
            // cannot masquerade as the preceding character.
            let hit = span.locate_utf16(utf16_offset, false)?;
            let leaf = MarkdownPieceTreeNode::Leaf(hit.piece);
            utf16_code_unit_in_tree(document, Some(&leaf), hit.local)
        }
        // Cached UTF-16 lengths locate the leaf without flattening the document.
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            if utf16_offset < left.metrics().utf16_len {
                // A strictly smaller position addresses a unit in the left child.
                utf16_code_unit_in_tree(document, Some(left.as_ref()), utf16_offset)
            } else {
                // The right child receives an offset relative to its own first unit.
                utf16_code_unit_in_tree(
                    document,
                    Some(right.as_ref()),
                    utf16_offset - left.metrics().utf16_len,
                )
            }
        }
    }
}

/// Copies the requested range into caller storage, pruning every disjoint
/// subtree without creating a whole-document UTF-16 mirror.
pub(super) fn copy_utf16_tree_range(
    document: &impl PieceByteSource,
    root: Option<&MarkdownPieceTreeNode>,
    range: Range<usize>,
    accumulated_utf16: usize,
    destination: &mut [u16],
    written: &mut usize,
) -> Result<(), MarkdownDocumentBufferError> {
    document.source_health()?;
    let Some(root) = root else {
        // Empty text contributes no bytes or code units and cannot invoke the output callback.
        return Ok(());
    };
    let node_start = accumulated_utf16;
    let node_end = node_start + root.metrics().utf16_len;
    if range.end <= node_start || range.start >= node_end {
        // A disjoint node contributes no units and requires no text decoding.
        return Ok(());
    }

    match root {
        // Clamp the overlap to this leaf and reserve only the corresponding destination slice.
        MarkdownPieceTreeNode::Leaf(piece) => document.with_piece_bytes(piece, |bytes| {
            let local_start = range.start.saturating_sub(node_start);
            let local_end = (range.end - node_start).min(piece.metrics.utf16_len);
            let requested_units = local_end.saturating_sub(local_start);
            let destination_end = written
                .checked_add(requested_units)
                .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
            let output = destination
                .get_mut(*written..destination_end)
                .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
            let text = std::str::from_utf8(bytes).expect("piece bytes remain valid UTF-8");
            for (slot, code_unit) in output
                .iter_mut()
                .zip(text.encode_utf16().skip(local_start).take(requested_units))
            {
                // Copy exactly the requested code units; partial surrogate ranges are valid NSString reads.
                *slot = code_unit;
            }
            *written = destination_end;
            Ok(())
        }),
        MarkdownPieceTreeNode::OriginalSpan(span) => {
            // Visit intersecting bounded pieces in directory order. Recursive calls
            // prune disjoint ranges before leasing at most 4 KiB of text.
            let mut base = accumulated_utf16;
            for ordinal in span.record_range() {
                let piece = span.piece(ordinal)?;
                let next = base + piece.metrics.utf16_len;
                let leaf = MarkdownPieceTreeNode::Leaf(piece);
                copy_utf16_tree_range(
                    document,
                    Some(&leaf),
                    range.clone(),
                    base,
                    destination,
                    written,
                )?;
                base = next;
            }
            Ok(())
        }
        // Visit children in document order so the mutable output cursor advances monotonically.
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            copy_utf16_tree_range(
                document,
                Some(left.as_ref()),
                range.clone(),
                accumulated_utf16,
                destination,
                written,
            )?;
            copy_utf16_tree_range(
                document,
                Some(right.as_ref()),
                range,
                accumulated_utf16 + left.metrics().utf16_len,
                destination,
                written,
            )
        }
    }
}

#[cfg(any(test, feature = "native-markdown-candidate"))]
/// Locate the byte after one logical line break while merging cross-leaf CRLF pairs.
pub(super) fn find_line_break_end(
    document: &MarkdownDocumentBuffer,
    root: &MarkdownPieceTreeNode,
    break_index: usize,
    accumulated_bytes: usize,
) -> Result<usize, MarkdownDocumentBufferError> {
    document.source_health()?;
    match root {
        // Scan only the selected leaf and distinguish paired from standalone line endings.
        MarkdownPieceTreeNode::Leaf(piece) => document.with_piece_bytes(piece, |bytes| {
            let mut seen = 0;
            let mut index = 0;
            while index < bytes.len() {
                // Each iteration consumes at least one byte and counts only logical line breaks.
                let break_end = match bytes[index] {
                    // A local CRLF is one break whose endpoint follows both bytes.
                    b'\r' if bytes.get(index + 1) == Some(&b'\n') => {
                        index += 2;
                        Some(index)
                    }
                    // A standalone CR or LF contributes one one-byte break.
                    b'\r' | b'\n' => {
                        index += 1;
                        Some(index)
                    }
                    // Ordinary UTF-8 bytes cannot be ASCII line-ending bytes and need no decoding here.
                    _ => {
                        index += 1;
                        None
                    }
                };
                if let Some(break_end) = break_end {
                    // Only a recognized line ending advances the logical break index.
                    if seen == break_index {
                        // The requested break has been reached; publish its document-relative endpoint.
                        return Ok(accumulated_bytes + break_end);
                    }
                    seen += 1;
                }
            }
            Err(MarkdownDocumentBufferError::RangeOutsideDocument)
        }),
        MarkdownPieceTreeNode::OriginalSpan(span) => {
            // Piece aggregates select one logical break. A cross-piece CRLF must
            // return the actual endpoint after LF.
            let mut prefix = MarkdownDocumentMetrics::default();
            for ordinal in span.record_range() {
                let piece = span.piece(ordinal)?;
                let joined = MarkdownDocumentMetrics::joined(prefix, piece.metrics);
                if break_index < joined.line_breaks {
                    let continued_lf = usize::from(
                        prefix.ends_with_carriage_return && piece.metrics.starts_with_line_feed,
                    );
                    let local = break_index - prefix.line_breaks + continued_lf;
                    if piece.metrics.ends_with_carriage_return
                        && local + 1 == piece.metrics.line_breaks
                        && ordinal + 1 < span.record_range().end
                        && span.piece(ordinal + 1)?.metrics.starts_with_line_feed
                    {
                        return Ok(accumulated_bytes + prefix.byte_len + piece.byte_len + 1);
                    } // Cross-piece CRLF is one logical break ending after LF.
                    return find_line_break_end(
                        document,
                        &MarkdownPieceTreeNode::Leaf(piece),
                        local,
                        accumulated_bytes + prefix.byte_len,
                    );
                }
                prefix = joined;
            }
            Err(MarkdownDocumentBufferError::RangeOutsideDocument)
        }
        // Use subtree counts but reconcile a CRLF split across the child boundary.
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            let left_metrics = left.metrics();
            let right_metrics = right.metrics();
            let boundary_is_crlf =
                left_metrics.ends_with_carriage_return && right_metrics.starts_with_line_feed;
            if boundary_is_crlf {
                // The left trailing CR and right leading LF represent a single shared line break.
                let independent_left_breaks = left_metrics.line_breaks - 1;
                if break_index < independent_left_breaks {
                    // Breaks before the shared CRLF remain wholly inside the left child.
                    return find_line_break_end(document, left, break_index, accumulated_bytes);
                }
                if break_index == independent_left_breaks {
                    // The shared CRLF ends one byte into the right child.
                    return Ok(accumulated_bytes + left_metrics.byte_len + 1);
                }
                return find_line_break_end(
                    document,
                    right,
                    break_index - independent_left_breaks,
                    accumulated_bytes + left_metrics.byte_len,
                );
            }
            if break_index < left_metrics.line_breaks {
                // Without a boundary CRLF, ordinary subtree break counts directly select the left side.
                find_line_break_end(document, left, break_index, accumulated_bytes)
            } else {
                // The right side receives the remaining break index after the complete left prefix.
                find_line_break_end(
                    document,
                    right,
                    break_index - left_metrics.line_breaks,
                    accumulated_bytes + left_metrics.byte_len,
                )
            }
        }
    }
}

#[cfg(any(test, feature = "native-markdown-candidate"))]
/// Aggregate a validated byte prefix using cached complete subtrees and one partial leaf.
pub(super) fn metrics_for_tree_prefix(
    document: &MarkdownDocumentBuffer,
    root: Option<&MarkdownPieceTreeNode>,
    byte_count: usize,
) -> Result<MarkdownDocumentMetrics, MarkdownDocumentBufferError> {
    document.source_health()?;
    let Some(root) = root else {
        // Empty storage has only its zero boundary; reject any positive requested extent below.
        return if byte_count == 0 {
            // An empty tree has a valid empty prefix with identity metrics.
            Ok(MarkdownDocumentMetrics::default())
        } else {
            // A nonempty prefix cannot be represented without a root.
            Err(MarkdownDocumentBufferError::RangeOutsideDocument)
        };
    };
    if byte_count == 0 {
        // The empty prefix requires no leaf decoding.
        return Ok(MarkdownDocumentMetrics::default());
    }
    if byte_count == root.metrics().byte_len {
        // A complete prefix can reuse all cached aggregates.
        return Ok(root.metrics());
    }
    if byte_count > root.metrics().byte_len {
        // Reject oversized prefixes before arithmetic or slicing.
        return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
    }
    match root {
        // Only a partial leaf requires rescanning its scalar-aligned prefix.
        MarkdownPieceTreeNode::Leaf(piece) => document.with_piece_bytes(piece, |bytes| {
            let text = std::str::from_utf8(bytes).expect("piece bytes remain valid UTF-8");
            if !text.is_char_boundary(byte_count) {
                // Prefix boundaries cannot divide a UTF-8 scalar.
                return Err(MarkdownDocumentBufferError::NotUnicodeBoundary);
            }
            Ok(MarkdownDocumentMetrics::from_text(&text[..byte_count]))
        }),
        MarkdownPieceTreeNode::OriginalSpan(span) => {
            // Complete pieces use cached metrics. Only the partial tail enters the
            // ordinary leaf path for Unicode validation and measurement.
            let hit = span.locate_byte(byte_count, true)?;
            let leaf = MarkdownPieceTreeNode::Leaf(hit.piece);
            let tail = metrics_for_tree_prefix(document, Some(&leaf), hit.local)?;
            Ok(MarkdownDocumentMetrics::joined(hit.prefix, tail))
        }
        // Complete child metrics avoid rescanning unaffected text.
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            if byte_count <= left.metrics().byte_len {
                // A prefix contained in the left child excludes the right child entirely.
                metrics_for_tree_prefix(document, Some(left.as_ref()), byte_count)
            } else {
                // A prefix entering the right child combines the complete left aggregate with a partial right one.
                let right_prefix = metrics_for_tree_prefix(
                    document,
                    Some(right.as_ref()),
                    byte_count - left.metrics().byte_len,
                )?;
                Ok(MarkdownDocumentMetrics::joined(
                    left.metrics(),
                    right_prefix,
                ))
            }
        }
    }
}

/// Borrows bounded pieces in text order inside the requested range. Every
/// visitor remains within the actual byte bounds of its one logical leaf.
pub(super) fn visit_piece_tree_range(
    document: &impl PieceByteSource,
    root: Option<&MarkdownPieceTreeNode>,
    range: Range<usize>,
    accumulated_bytes: usize,
    visit: &mut impl FnMut(&[u8]) -> Result<(), MarkdownDocumentBufferError>,
) -> Result<(), MarkdownDocumentBufferError> {
    // Full-range consumers retain their established unit-result contract. They
    // use the same actual traversal as early-stopping readers, always continuing
    // after a successful callback rather than maintaining a second algorithm.
    visit_piece_tree_range_until_break(document, root, range, accumulated_bytes, &mut |bytes| {
        visit(bytes).map(|()| ControlFlow::Continue(()))
    })
    .map(|_| ())
}

/// Visit original bounded leaves until the consumer explicitly breaks. A normal
/// break propagates only after the current actual source lease finishes; source
/// and callback failures remain typed errors, never disguised traversal control.
pub(super) fn visit_piece_tree_range_until_break(
    document: &impl PieceByteSource,
    root: Option<&MarkdownPieceTreeNode>,
    range: Range<usize>,
    accumulated_bytes: usize,
    visit: &mut impl FnMut(&[u8]) -> Result<ControlFlow<()>, MarkdownDocumentBufferError>,
) -> Result<ControlFlow<()>, MarkdownDocumentBufferError> {
    document.source_health()?;
    let Some(root) = root else {
        // Empty text contributes no bytes or code units and cannot invoke the output callback.
        return Ok(ControlFlow::Continue(()));
    };
    let node_start = accumulated_bytes;
    let node_end = node_start + root.metrics().byte_len;
    if range.end <= node_start || range.start >= node_end {
        // Prune disjoint subtrees before borrowing any backing bytes.
        return Ok(ControlFlow::Continue(()));
    }
    match root {
        // Clamp the overlap to the selected leaf and lend it only for this callback.
        MarkdownPieceTreeNode::Leaf(piece) => document.with_piece_bytes(piece, |bytes| {
            let local_start = range.start.saturating_sub(node_start);
            let local_end = (range.end - node_start).min(piece.byte_len);
            visit(&bytes[local_start..local_end])
        }),
        MarkdownPieceTreeNode::OriginalSpan(span) => {
            // A span never becomes one oversized text slice; each actual callback
            // still comes from a real bounded logical piece.
            let mut base = accumulated_bytes;
            for ordinal in span.record_range() {
                let piece = span.piece(ordinal)?;
                let next = base + piece.byte_len;
                let leaf = MarkdownPieceTreeNode::Leaf(piece);
                let progress = visit_piece_tree_range_until_break(
                    document,
                    Some(&leaf),
                    range.clone(),
                    base,
                    visit,
                )?;
                if progress.is_break() {
                    // The matched leaf already released its lease. Stop before
                    // reading another span record or acquiring its source bytes.
                    return Ok(progress);
                }
                base = next;
            }
            Ok(ControlFlow::Continue(()))
        }
        // Visit the left range first, propagating callback failures before entering the right side.
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            let progress = visit_piece_tree_range_until_break(
                document,
                Some(left.as_ref()),
                range.clone(),
                node_start,
                visit,
            )?;
            if progress.is_break() {
                // A completed left-side result excludes the right subtree;
                // no later leaf is borrowed merely to return an unused callback result.
                return Ok(progress);
            }
            visit_piece_tree_range_until_break(
                document,
                Some(right.as_ref()),
                range,
                node_start + left.metrics().byte_len,
                visit,
            )
        }
    }
}

/// Stream the live document into a writer without allocating a document-sized String.
pub(super) fn write_piece_tree(
    document: &MarkdownDocumentBuffer,
    root: Option<&MarkdownPieceTreeNode>,
    destination: &mut impl Write,
) -> Result<(), MarkdownDocumentBufferError> {
    document.source_health()?;
    let Some(root) = root else {
        // Empty text contributes no bytes or code units and cannot invoke the output callback.
        return Ok(());
    };
    match root {
        // Write an immutable backing slice directly; I/O failure aborts the caller's save transaction.
        MarkdownPieceTreeNode::Leaf(piece) => document.with_piece_bytes(piece, |bytes| {
            destination.write_all(bytes).map_err(|_| {
                // Writer failure preserves the source model; actual lease cleanup
                // completes before this error propagates.
                MarkdownDocumentBufferError::WriteUnavailable
            })
        }),
        MarkdownPieceTreeNode::OriginalSpan(span) => {
            // Save writes original bounded pieces directly. Stop on the first write
            // or lease-cleanup failure, preserving save-transaction semantics.
            for ordinal in span.record_range() {
                write_piece_tree(
                    document,
                    Some(&MarkdownPieceTreeNode::Leaf(span.piece(ordinal)?)),
                    destination,
                )?;
            }
            Ok(())
        }
        // Preserve text order and stop at the first failed child write.
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            write_piece_tree(document, Some(left.as_ref()), destination)?;
            write_piece_tree(document, Some(right.as_ref()), destination)
        }
    }
}

/// Feed live bytes into a revision hash without copying or changing tree ownership.
pub(super) fn hash_piece_tree(
    document: &impl PieceByteSource,
    root: Option<&MarkdownPieceTreeNode>,
    hasher: &mut Sha256,
) -> Result<(), MarkdownDocumentBufferError> {
    document.source_health()?;
    let Some(root) = root else {
        // An empty tree reads no text and is still the real empty SHA-256 input.
        return Ok(());
    };
    match root {
        // Hash the leaf's exact stored bytes, preserving the document's original line endings.
        MarkdownPieceTreeNode::Leaf(piece) => document.with_piece_bytes(piece, |bytes| {
            hasher.update(bytes);
            Ok(())
        }),
        MarkdownPieceTreeNode::OriginalSpan(span) => {
            // Hash bounded pieces in original byte order. Compressed metadata and
            // tree shape never enter the document revision.
            for ordinal in span.record_range() {
                hash_piece_tree(
                    document,
                    Some(&MarkdownPieceTreeNode::Leaf(span.piece(ordinal)?)),
                    hasher,
                )?;
            }
            Ok(())
        }
        // Hash children in text order so tree shape never affects the revision.
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            hash_piece_tree(document, Some(left.as_ref()), hasher)?;
            hash_piece_tree(document, Some(right.as_ref()), hasher)
        }
    }
}

/// Return the root aggregate or the empty-document identity in constant time.
pub(super) fn piece_tree_metrics(root: &PieceTreeLink) -> MarkdownDocumentMetrics {
    root.as_ref()
        .map_or(MarkdownDocumentMetrics::default(), |root| {
            // Use the cached nonempty root aggregate without rescanning source bytes.
            root.metrics()
        })
}

#[cfg(test)]
/// Expose the cached tree height for balance regression assertions.
pub(super) fn piece_tree_height(root: &PieceTreeLink) -> u16 {
    root.as_ref().map_or(0, |root| {
        // Use the cached root height for structural regression measurements.
        root.height()
    })
}

#[cfg(test)]
/// Count retained leaves recursively for structural sharing regression tests.
pub(super) fn piece_tree_leaf_count(root: Option<&MarkdownPieceTreeNode>) -> usize {
    let Some(root) = root else {
        // Empty documents own no leaf allocations.
        return 0;
    };
    match root {
        // A leaf contributes one immutable piece regardless of byte length.
        MarkdownPieceTreeNode::Leaf(_) => 1,
        MarkdownPieceTreeNode::OriginalSpan(_) => 1,
        // Count both disjoint child trees to measure the full persistent shape.
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            piece_tree_leaf_count(Some(left.as_ref())) + piece_tree_leaf_count(Some(right.as_ref()))
        }
    }
}

/// Compare persistent root identity to recognize an unchanged saved document.
pub(super) fn piece_tree_links_share_root(left: &PieceTreeLink, right: &PieceTreeLink) -> bool {
    match (left, right) {
        // Two absent roots represent the same empty tree.
        (None, None) => true,
        // Pointer equality recognizes the exact saved root without rescanning text.
        (Some(left), Some(right)) => Arc::ptr_eq(left, right),
        // An empty and nonempty root cannot represent an unchanged tree.
        _ => false,
    }
}
