//! Immutable text values share only their persistent tree and referenced byte
//! resources, without extending the active document or history lifetime.

use std::collections::HashMap;
use std::sync::Mutex;

use super::*;

#[cfg(feature = "paged-shaping-frontier")]
#[path = "bidi_paragraph.rs"]
mod bidi_paragraph;
#[path = "printable_ascii.rs"]
mod printable_ascii;
#[path = "text_checking_range.rs"]
mod text_checking_range;
#[cfg(feature = "paged-shaping-frontier")]
pub(crate) use bidi_paragraph::{FrozenBidiParagraphCursor, FrozenBidiParagraphStep};

#[cfg(feature = "paged-shaping-frontier")]
pub(crate) use printable_ascii::{FrozenPrintableAsciiSuffix, FrozenPrintableAsciiSuffixStep};

#[derive(Clone)]
/// An immutable current-text value whose clone adds one Arc, never a UTF-8 or
/// UTF-16 body copy.
///
/// Original bytes use an independently frozen anonymous backing; edited bytes use
/// immutable shared segments. Later edits, undo/redo, and active-document teardown
/// cannot change this value. Its exclusively retained resources end with the last snapshot.
pub struct MarkdownTextSnapshot {
    text: Arc<FrozenText>,
}

/// Retain only read resources: the captured root, exact anonymous payload range,
/// and edit segments that this root actually references.
///
/// Sparse keys preserve original segment IDs without retaining deleted segments
/// needed only by undo history merely to keep an index contiguous.
struct FrozenText {
    #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
    projection: Option<Arc<super::display_projection::DisplayProjection>>,
    root: PieceTreeLink,
    original_snapshot: Option<Arc<OriginalBacking>>,
    edits: HashMap<usize, Arc<MarkdownEditSegment>>,
    // List only this root's actual file resources. Heap bytes have no I/O health
    // failure, so hot reads need not walk the entire sparse segment map.
    edit_backings: Vec<Arc<OriginalBacking>>,
    // All captures/clones from one document share a single derived window,
    // without sharing ownership of the active document itself.
    utf16_read_cache: Arc<Mutex<read_cache::Utf16ReadCache>>,
    // The captured generation participates in the cache key. Interleaved old/live
    // reads cannot bless an old value using the active document's newer generation.
    generation: u64,
    // The draft guard retains no document; canonical captures have none. Its
    // real read claim must outlive the tree, segments, and health-owning backings.
    #[cfg(feature = "s2-table-window-prototype")]
    draft_guard: Option<cell_draft::FrozenDraftGuard>,
    #[cfg(feature = "s2-table-window-prototype")]
    _draft_read_claim: Option<cell_draft::ReadClaim>,
}

impl MarkdownTextSnapshot {
    /// Bind physical table rows only to the current canonical allocation. Equal
    /// bytes from another document, old generations, and display projections confer no authority.
    #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
    pub(crate) fn shares_current_table_root(&self, document: &MarkdownDocumentBuffer) -> bool {
        self.text.projection.is_none()
            && self.text.generation == document.generation
            && self.text.draft_guard.is_none()
            && self.text.root.as_ref().is_some_and(|root| {
                document
                    .root
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(root, current))
            })
    }
    /// Distinguish the piece tree from a display directory explicitly; a
    /// projection's absent tree root never means its document is empty.
    #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
    pub(super) fn display_projection(
        &self,
    ) -> Option<&Arc<super::display_projection::DisplayProjection>> {
        self.text.projection.as_ref()
    }

    /// Capture an actually sealed display directory without an active document,
    /// copied body, or duplicate display AVL nodes.
    #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
    pub(crate) fn from_display_projection(
        projection: Arc<super::display_projection::DisplayProjection>,
        generation: u64,
    ) -> Self {
        Self {
            text: Arc::new(FrozenText {
                projection: Some(projection),
                root: None,
                original_snapshot: None,
                edits: HashMap::new(),
                edit_backings: Vec::new(),
                utf16_read_cache: Arc::new(Mutex::new(read_cache::Utf16ReadCache::default())),
                generation,
                draft_guard: None,
                _draft_read_claim: None,
            }),
        }
    }

    /// Obtain aggregates from the same source that supplies bytes. Display values
    /// use the actual visible-scalar facts accumulated while that projection was built.
    fn metrics(&self) -> MarkdownDocumentMetrics {
        #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
        if let Some(projection) = self.display_projection() {
            // A projection owns its own exact aggregate, not the absent canonical tree.
            return projection.metrics();
        }
        piece_tree_metrics(&self.text.root)
    }
    /// Establish only the captured tree path; actual mixed-endpoint byte reads
    /// remain charged to the caller's later work quantum.
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn begin_printable_ascii_suffix(
        &self,
        range: Range<usize>,
    ) -> Result<FrozenPrintableAsciiSuffix, MarkdownDocumentBufferError> {
        self.text.source_health()?;
        if range.start > range.end || range.end > self.utf16_len() {
            // Reject invalid coordinates before building a source cursor.
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        Ok(FrozenPrintableAsciiSuffix::new(self, range))
    }

    /// Check the same frozen resources before/after work; return exact suffix
    /// facts only after that step's actual leases have ended.
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn advance_printable_ascii_suffix(
        &self,
        cursor: &mut FrozenPrintableAsciiSuffix,
        bytes: usize,
        scalars: usize,
        metadata: usize,
    ) -> Result<FrozenPrintableAsciiSuffixStep, MarkdownDocumentBufferError> {
        self.text.source_health()?;
        let result = cursor.advance(self, bytes, scalars, metadata);
        self.text.source_health()?;
        result
    }

    /// Bind a real bidi-paragraph query to the same immutable root. Construction
    /// reads no endpoint leaf; advance pays its own fine-grained source work.
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn begin_bidi_paragraph(
        &self,
        position: usize,
    ) -> Result<FrozenBidiParagraphCursor, MarkdownDocumentBufferError> {
        self.text.source_health()?;
        if position > self.utf16_len() {
            // EOF is a valid query position, but a position beyond it is not.
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        Ok(FrozenBidiParagraphCursor::new(self, position))
    }

    /// Validate the original frozen resources around each step. Candidate
    /// endpoints cannot escape before every actual lease in that step is reclaimed.
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn advance_bidi_paragraph(
        &self,
        cursor: &mut FrozenBidiParagraphCursor,
        bytes: usize,
        scalars: usize,
        metadata: usize,
    ) -> Result<FrozenBidiParagraphStep, MarkdownDocumentBufferError> {
        self.text.source_health()?;
        let result = cursor.advance(self, bytes, scalars, metadata);
        self.text.source_health()?;
        result
    }

    /// Capture the current canonical root, segments, and original generation together.
    pub(super) fn capture(document: &MarkdownDocumentBuffer) -> Self {
        Self::capture_root(
            document,
            &document.root,
            &document.edits,
            document.generation,
        )
    }

    /// Only a controlled same-module candidate may supply another root/segment
    /// table; ordinary Source callers have no such constructor authority. Reuse
    /// resource collection without giving a draft duplicate implementation or canonical tickets.
    pub(super) fn capture_root(
        document: &MarkdownDocumentBuffer,
        root: &PieceTreeLink,
        edits: &MarkdownEditSegments,
        generation: u64,
    ) -> Self {
        let mut text = FrozenText {
            #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
            projection: None,
            root: root.clone(),
            original_snapshot: None,
            edits: HashMap::new(),
            edit_backings: Vec::new(),
            utf16_read_cache: Arc::clone(&document.utf16_read_cache),
            generation,
            #[cfg(feature = "s2-table-window-prototype")]
            draft_guard: None,
            #[cfg(feature = "s2-table-window-prototype")]
            _draft_read_claim: None,
        };
        // Traverse source metadata only: no body read, decode, copy, history
        // traversal, or change-journal access belongs to snapshot acquisition.
        retain_piece_sources(root.as_deref(), document, edits, &mut text);
        Self {
            text: Arc::new(text),
        }
    }

    /// Share the original tree slice for a read-only table cell. Even an empty or
    /// all-edited cell retains the original document source's actual I/O health fact.
    #[cfg(all(
        feature = "s2-table-window-prototype",
        any(
            feature = "paged-graphemes",
            feature = "paged-line-breaks",
            feature = "paged-shaping"
        )
    ))]
    pub(super) fn capture_table_cell_root(
        document: &MarkdownDocumentBuffer,
        root: &PieceTreeLink,
    ) -> Self {
        let mut snapshot = Self::capture_root(document, root, &document.edits, document.generation);
        let text = Arc::get_mut(&mut snapshot.text).expect("新只读格 Frozen 尚未共享");
        if text.original_snapshot.is_none() {
            // This cell ticket came from the original table structure. A slice
            // without Original pieces cannot turn an existing source failure into success.
            text.original_snapshot = document.original_snapshot.clone();
        }
        snapshot
    }

    /// Install the original draft guard on the sole new Arc after building this
    /// same frozen value. Ordinary Source callers cannot supply these closed parameters.
    #[cfg(feature = "s2-table-window-prototype")]
    pub(super) fn capture_draft_root(
        document: &MarkdownDocumentBuffer,
        root: &PieceTreeLink,
        edits: &MarkdownEditSegments,
        content_serial: u64,
        guard: cell_draft::FrozenDraftGuard,
        claim: cell_draft::ReadClaim,
    ) -> Self {
        let mut snapshot = Self::capture_root(document, root, edits, content_serial);
        let text = Arc::get_mut(&mut snapshot.text).expect("新Frozen尚未共享");
        text.draft_guard = Some(guard);
        text._draft_read_claim = Some(claim);
        snapshot
    }

    /// Lend only the original scope's weak authority to background metadata work;
    /// it cannot capture a root or bypass the Source boundary.
    #[cfg(feature = "s2-table-window-prototype")]
    pub(crate) fn cell_draft_source_authority(
        &self,
    ) -> Option<cell_draft::MarkdownCellDraftSourceAuthority> {
        self.text
            .draft_guard
            .as_ref()
            .map(|guard| guard.authority())
    }

    /// Observe current health of this captured value's original and edited resources.
    pub(crate) fn original_backing_health(&self) -> Result<(), MarkdownDocumentBufferError> {
        self.text.source_health()
    }
    /// Observe atomic original-backing health without retaining its bytes. Drafts
    /// preserve their original health owner; an all-edited canonical value may have none.
    #[must_use]
    pub fn original_backing_observer(&self) -> Option<crate::MarkdownOriginalBackingObserver> {
        #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
        if let Some(projection) = self.display_projection() {
            // The projection's raw source owns the original backing observation.
            return projection.raw().original_backing_observer();
        }
        let observer = self
            .text
            .original_snapshot
            .as_ref()
            .map(|backing| backing.observer());
        #[cfg(feature = "s2-table-window-prototype")]
        if observer.is_none() {
            // An all-edited draft still belongs to its original Source health
            // owner. Lend that atomic observation without inventing a missing FD.
            return self
                .text
                .draft_guard
                .as_ref()
                .and_then(|guard| guard.original_backing_observer());
        }
        observer
    }

    #[must_use]
    /// Read the captured source's exact cached byte aggregate without scanning text.
    pub fn byte_len(&self) -> usize {
        self.metrics().byte_len
    }

    #[must_use]
    /// Read the exact cached UTF-16 aggregate without constructing an encoding mirror.
    pub fn utf16_len(&self) -> usize {
        self.metrics().utf16_len
    }

    /// Equal scalar and UTF-16 counts prove no surrogate pairs. This establishes
    /// UTF-16 scalar boundaries only, not grapheme or paragraph boundaries.
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn utf16_boundaries_are_scalar(&self) -> Result<bool, MarkdownDocumentBufferError> {
        self.text.source_health()?;
        let metrics = self.metrics();
        let dense = metrics.character_count == metrics.utf16_len;
        self.text.source_health()?;
        Ok(dense)
    }

    /// Locate a text-checking paragraph for an already-confined request, returning
    /// only global UTF-16 coordinates rather than materialized paragraph text.
    ///
    /// CR, LF, and U+2029 terminate paragraphs; NEL/U+2028 remain inside them.
    /// Nonempty requests locate the final paragraph using the last selected unit.
    /// CR includes its following LF, while a caret on that LF keeps the LF-only range.
    /// Tree aggregates skip text between endpoints; decode at most one 4 KiB hit
    /// leaf per endpoint plus one CR successor. Low-surrogate requests are valid
    /// observations, without granting edit or whole-paragraph materialization authority.
    ///
    /// # Errors
    ///
    /// Reversed or beyond-EOF requests fail before accessing piece bytes. The
    /// native caller owns NSNotFound/partially out-of-bounds adjustment; this
    /// query neither guesses that intent nor silently truncates the supplied extent.
    pub fn text_checking_paragraph_range(
        &self,
        request: Range<usize>,
    ) -> Result<Range<usize>, MarkdownDocumentBufferError> {
        self.text.source_health()?;
        #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
        if let Some(projection) = self.display_projection() {
            return projection.text_checking_paragraph_range(request);
        }
        text_checking_range::paragraph_range(self.text.as_ref(), self.text.root.as_deref(), request)
    }

    /// Determine exactly whether this frozen source's UTF-16 range is U+0020..U+007E.
    ///
    /// Entirely ASCII subtrees need only cached aggregates; each partial mixed
    /// boundary leaf borrows at most 4 KiB. Never copy/scan the entire range or
    /// reread boundary leaves already proven ASCII for another item/style interval.
    /// Empty ranges are true only at valid scalar boundaries, not inside a surrogate pair.
    ///
    /// # Errors
    /// Reject reversed, outside, or split-surrogate coordinates. Real source
    /// health is checked around the query and takes precedence over its predicate result.
    pub fn is_printable_ascii_range(
        &self,
        range: Range<usize>,
    ) -> Result<bool, MarkdownDocumentBufferError> {
        #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
        if let Some(projection) = self.display_projection() {
            return projection.is_printable_ascii_range(range);
        }
        printable_ascii::query(self.text.as_ref(), self.text.root.as_deref(), range)
    }

    /// Establish a word cursor only with exact all-source ASCII proof. The
    /// original layout owner supplies Source authority; do not recapture another value.
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn begin_ascii_word_run(
        &self,
        position: usize,
        forward: bool,
        authority: crate::MarkdownLayoutTextAuthority,
    ) -> Result<MarkdownAsciiWordRun, MarkdownDocumentBufferError> {
        self.text.source_health()?;
        #[cfg(feature = "s2-table-window-prototype")]
        if let Some(projection) = self.display_projection() {
            // A display source uses its own sealed printable aggregate and
            // coordinate space, without pretending to have a canonical tree root.
            if position > self.utf16_len()
                || !projection.metrics().all_printable_ascii.is_printable()
            {
                // Neither an out-of-range position nor nonprintable display text
                // can authorize this specialized ASCII cursor.
                return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
            }
            return Ok(MarkdownAsciiWordRun::new_display(
                position, forward, authority,
            ));
        }
        if position > self.utf16_len()
            || !self
                .text
                .root
                .as_ref()
                .is_none_or(|root| root.metrics().all_printable_ascii.is_printable())
        {
            // The canonical path requires the same exact printable aggregate;
            // empty-root text is vacuously ASCII but still respects its zero extent.
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        let run = MarkdownAsciiWordRun::new(&self.text.root, position, forward, authority);
        self.text.source_health()?;
        Ok(run)
    }

    /// Permit a display directory's equal-length contiguous raw run only. Its
    /// sealed display aggregate already proved ASCII; construction performs no rescan.
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    pub(super) fn begin_scoped_ascii_word_run(
        &self,
        position: usize,
        forward: bool,
        authority: crate::MarkdownLayoutTextAuthority,
        range: Range<usize>,
    ) -> Result<MarkdownAsciiWordRun, MarkdownDocumentBufferError> {
        self.text.source_health()?;
        if self.display_projection().is_some()
            || range.start > position
            || position > range.end
            || range.end > self.utf16_len()
        {
            // Scoped raw runs cannot be derived from another projection or
            // from an extent that excludes the requested original position.
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        Ok(MarkdownAsciiWordRun::new_range(
            &self.text.root,
            position,
            forward,
            authority,
            range,
        ))
    }

    /// Check actual health around reads of this same frozen payload. Each step
    /// consumes only the caller's remaining byte, scalar, and metadata quantum.
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn advance_ascii_word_run(
        &self,
        run: &mut MarkdownAsciiWordRun,
        bytes: usize,
        scalars: usize,
        metadata: usize,
    ) -> Result<MarkdownAsciiWordStep, MarkdownDocumentBufferError> {
        self.text.source_health()?;
        #[cfg(feature = "s2-table-window-prototype")]
        if let Some(projection) = self.display_projection() {
            let result = run.advance_display(projection, bytes, scalars, metadata);
            self.text.source_health()?;
            return result;
        }
        let result = run.advance(self.text.as_ref(), bytes, scalars, metadata);
        self.text.source_health()?;
        result
    }

    /// Read this entire captured root's exact letter fact: None means control or
    /// Unicode content; Some distinguishes printable ASCII with/without letters.
    /// Use aggregates maintained by real leaf creation/editing, with no body lease,
    /// range scan, or authority borrowed from a later snapshot.
    #[cfg(any(feature = "paged-shaping-frontier", test))]
    pub(crate) fn printable_ascii_has_letters(
        &self,
    ) -> Result<Option<bool>, MarkdownDocumentBufferError> {
        self.text.source_health()?;
        let class = self.metrics().all_printable_ascii;
        // A positive aggregate cannot conceal a backing failure that arrived during the query.
        self.text.source_health()?;
        Ok(class.has_letters())
    }

    /// Read the complete Unicode property set maintained for this frozen root at
    /// validated open and real edits. Capture no newer snapshot and borrow no
    /// text; a positive aggregate cannot conceal a source failure around the read.
    #[cfg(any(feature = "paged-shaping-frontier", test))]
    pub(crate) fn unicode_properties(
        &self,
    ) -> Result<MarkdownUnicodeProperties, MarkdownDocumentBufferError> {
        self.text.source_health()?;
        let properties = self.metrics().unicode_properties;
        self.text.source_health()?;
        Ok(properties)
    }

    /// Map a UTF-16 scalar boundary into this same frozen value's byte space,
    /// explicitly rejecting positions inside a surrogate pair.
    pub fn byte_offset_for_utf16(
        &self,
        utf16_offset: usize,
    ) -> Result<usize, MarkdownDocumentBufferError> {
        self.text.source_health()?;
        #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
        if let Some(projection) = self.display_projection() {
            return projection.byte_offset_for_utf16(utf16_offset);
        }
        if utf16_offset > self.utf16_len() {
            // Reject outside positions before descending; real EOF remains a
            // usable boundary for an empty range.
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        byte_offset_for_utf16_in_tree(
            self.text.as_ref(),
            self.text.root.as_deref(),
            utf16_offset,
            0,
        )
    }

    /// Map a UTF-8 scalar boundary to this captured value's UTF-16 coordinate;
    /// a byte inside a multibyte scalar is not a valid boundary.
    pub fn utf16_offset_for_byte(
        &self,
        byte_offset: usize,
    ) -> Result<usize, MarkdownDocumentBufferError> {
        self.text.source_health()?;
        #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
        if let Some(projection) = self.display_projection() {
            return projection.utf16_offset_for_byte(byte_offset);
        }
        if byte_offset > self.byte_len() {
            // Check the full extent before a leaf decoder could borrow a nonexistent prefix.
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        utf16_offset_for_byte_in_tree(
            self.text.as_ref(),
            self.text.root.as_deref(),
            byte_offset,
            0,
        )
    }

    /// Read one NSString-compatible UTF-16 unit, including a low surrogate;
    /// real EOF is a boundary but not a readable unit.
    pub fn utf16_code_unit_at(
        &self,
        utf16_offset: usize,
    ) -> Result<u16, MarkdownDocumentBufferError> {
        // A failed anonymous backing revokes even a previously cached unit;
        // cache presence cannot restore source-reading authority.
        self.text.source_health()?;
        if utf16_offset >= self.utf16_len() {
            // Reject EOF and outside positions before requesting a cache window.
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        let mut unit = [0];
        self.copy_utf16_code_units(utf16_offset, &mut unit)?;
        Ok(unit[0])
    }

    /// Fill the caller's complete requested extent while every snapshot clone
    /// shares one at-most-8-KiB read-cache window.
    ///
    /// Split-surrogate ranges remain valid unit reads. A cache window is published
    /// only after complete fill and actual lease reclamation. Lock poisoning is
    /// an explicit error, never permission to clear the lock, expose a partial
    /// window, or fall back to a paragraph/whole-document copy.
    pub fn copy_utf16_code_units(
        &self,
        utf16_offset: usize,
        destination: &mut [u16],
    ) -> Result<(), MarkdownDocumentBufferError> {
        self.text.source_health()?;
        #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
        if let Some(projection) = self.display_projection() {
            return projection.copy_utf16_code_units(utf16_offset, destination);
        }
        let mut cache = self
            .text
            .utf16_read_cache
            .lock()
            .map_err(|_| MarkdownDocumentBufferError::Utf16ReadCacheUnavailable)?;
        // A shared cache hit needs this captured root and its original generation
        // together. Another capture may replace only the derived cached value.
        cache.copy(
            self.text.as_ref(),
            &self.text.root,
            self.text.generation,
            utf16_offset,
            destination,
        )
    }

    /// Visit finite pieces of an explicit byte range in order. A callback borrows
    /// at most the original 4 KiB leaf extent without a body copy.
    ///
    /// Original UTF-8 byte addressing permits partial-scalar observations. Callers
    /// needing text boundaries must validate coordinates first. Callback failure
    /// stops before the next leaf and its result is never retained as a cache.
    pub fn visit_byte_range(
        &self,
        range: Range<usize>,
        mut visit: impl FnMut(&[u8]) -> Result<(), MarkdownDocumentBufferError>,
    ) -> Result<(), MarkdownDocumentBufferError> {
        self.text.source_health()?;
        #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
        if let Some(projection) = self.display_projection() {
            return projection.visit_byte_range(range, &mut visit);
        }
        if range.start > range.end || range.end > self.byte_len() {
            // Reject reversed/outside extents before the first callback; an
            // empty text value permits only its actual 0..0 range.
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        visit_piece_tree_range(
            self.text.as_ref(),
            self.text.root.as_deref(),
            range,
            0,
            &mut visit,
        )
    }
}

/// Collect only resources actually referenced by this root. Reuse its original
/// tree/body Arcs without rebuilding leaves or scanning content.
fn retain_piece_sources(
    root: Option<&MarkdownPieceTreeNode>,
    document: &MarkdownDocumentBuffer,
    edits: &MarkdownEditSegments,
    frozen: &mut FrozenText,
) {
    let Some(root) = root else {
        // An empty root needs no text resources. Active-document history alone
        // cannot drag its retained mappings into this immutable text value.
        return;
    };
    match root {
        MarkdownPieceTreeNode::OriginalSpan(_) => {
            // A span compresses only original-piece metadata. Share its same
            // anonymous payload without retaining the document or creating another map.
            if frozen.original_snapshot.is_none() {
                frozen.original_snapshot = Some(Arc::clone(
                    document
                        .original_snapshot
                        .as_ref()
                        .expect("原始Span必须有不可变映射"),
                ));
            }
        }
        MarkdownPieceTreeNode::Leaf(piece) => match piece.source {
            MarkdownPieceSource::Original => {
                if frozen.original_snapshot.is_none() {
                    // All original leaves share the one independently frozen
                    // backing without copying or remapping their content.
                    frozen.original_snapshot = Some(Arc::clone(
                        document
                            .original_snapshot
                            .as_ref()
                            .expect("an original piece requires its immutable mapping"),
                    ));
                }
            }
            MarkdownPieceSource::EditSegment(index) => {
                // Split leaves may reference the same segment; retain its Arc
                // once under the original stable segment ID.
                frozen.edits.entry(index).or_insert_with(|| {
                    let segment = &edits.segments[index];
                    if let MarkdownEditSegment::Frozen(backing) = segment.as_ref() {
                        // Register a backing only at its first actual reference;
                        // this is the frozen value's strong ownership of that resource.
                        frozen.edit_backings.push(Arc::clone(backing));
                    }
                    Arc::clone(segment)
                });
            }
        },
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            // AVL height bounds recursion. Traversal order affects metadata
            // collection only and causes no content reads.
            retain_piece_sources(Some(left.as_ref()), document, edits, frozen);
            retain_piece_sources(Some(right.as_ref()), document, edits, frozen);
        }
    }
}

impl PieceByteSource for FrozenText {
    /// Check original and actually referenced edit resources before draft content
    /// authority, preserving their real failure priority without retaining a live document.
    fn source_health(&self) -> Result<(), MarkdownDocumentBufferError> {
        #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
        if let Some(projection) = &self.projection {
            // Projection reads belong to its own sealed directory and raw source.
            return projection.health();
        }
        #[cfg(feature = "s2-table-window-prototype")]
        let guarded = if let Some(guard) = &self.draft_guard {
            // Controlled capture binds the guard and Original leaves to the
            // same document backing; all-edited raw text still preserves Source health.
            guard.backing_health()?;
            true
        } else {
            // Canonical captures have no additional draft content authority.
            false
        };
        #[cfg(not(feature = "s2-table-window-prototype"))]
        let guarded = false;
        if !guarded && let Some(backing) = self.original_snapshot.as_deref() {
            // Keep original-backing failure priority; healthy edits cannot replace it.
            backing.health()?;
        }
        for backing in &self.edit_backings {
            // Check only this root's referenced resources. A real sealed-segment
            // failure also records the same draft's source cause, without owning its document.
            backing
                .health()
                .map_err(MarkdownDocumentBufferError::from)
                .inspect_err(|&error| {
                    #[cfg(feature = "s2-table-window-prototype")]
                    if let Some(guard) = &self.draft_guard {
                        // Preserve the original source failure for later consumers
                        // of this draft, rather than allowing a partial healthy result.
                        guard.record_source(error);
                    }
                    #[cfg(not(feature = "s2-table-window-prototype"))]
                    let _ = error;
                })?;
        }
        #[cfg(feature = "s2-table-window-prototype")]
        if let Some(guard) = &self.draft_guard {
            // After all real backings prove healthy, check content authority
            // alone without re-reading the same original backing.
            guard.require_content()?;
        }
        Ok(())
    }
    /// Use the same range reader for an entire already-bounded logical piece.
    fn piece_bytes(
        &self,
        piece: &MarkdownPiece,
    ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError> {
        self.read_piece_range(piece, 0..piece.byte_len)
    }

    /// A word query requests its actual local subrange of the original leaf;
    /// whole-piece callers retain the same established entry point above.
    #[cfg(feature = "paged-shaping-frontier")]
    fn piece_range_bytes(
        &self,
        piece: &MarkdownPiece,
        local: Range<usize>,
    ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError> {
        self.read_piece_range(piece, local)
    }

    /// End the actual source lease before exposing a callback result, then
    /// recheck any draft authority that the callback itself could have revoked.
    fn with_piece_bytes<R>(
        &self,
        piece: &MarkdownPiece,
        visit: impl FnOnce(&[u8]) -> Result<R, MarkdownDocumentBufferError>,
    ) -> Result<R, MarkdownDocumentBufferError> {
        let bytes = self.piece_bytes(piece)?;
        let result = visit(&bytes);
        // Lease reclamation currently takes precedence over the callback result.
        // Even the final callback can revoke a draft; no next leaf exists to
        // perform that required authority check on its behalf.
        bytes.finish()?;
        #[cfg(feature = "s2-table-window-prototype")]
        if self.draft_guard.is_some() {
            self.source_health()?;
        }
        result
    }
}

impl FrozenText {
    /// Narrow only the actual lease, never fabricate a piece whose aggregate
    /// disagrees with its bytes. Whole-piece and word reads share this resource boundary.
    fn read_piece_range(
        &self,
        piece: &MarkdownPiece,
        local: Range<usize>,
    ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError> {
        self.source_health()?;
        if local.start > local.end || local.end > piece.byte_len {
            // A local lease cannot escape or invert the original logical piece extent.
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        match piece.source {
            MarkdownPieceSource::Original => {
                // Original piece offsets refer directly to this same immutable Markdown snapshot.
                // Retain its actual health checks and acquire only the requested finite local lease.
                let backing = self
                    .original_snapshot
                    .as_ref()
                    .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
                let start = piece
                    .byte_start
                    .checked_add(local.start)
                    .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
                backing
                    .lease(start..start + local.len())
                    .map(PieceBytes::Original)
                    .map_err(Into::into)
            }
            MarkdownPieceSource::EditSegment(index) => {
                // The sparse map contains only original IDs referenced by this
                // root. Read its immutable segment rather than the active document's table.
                let segment = self
                    .edits
                    .get(&index)
                    .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
                segment.read(
                    piece.byte_start + local.start..piece.byte_start + local.end,
                    true,
                )
            }
        }
    }
}

#[cfg(test)]
mod tests;
