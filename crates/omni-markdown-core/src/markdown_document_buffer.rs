//! Active mmap/piece-tree document API, edit history, and bounded viewport access.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs::{Metadata, OpenOptions};
use std::io::Write;
use std::ops::Range;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::original_backing::OriginalBacking;

use crate::immutable_snapshot::{SnapshotError, snapshot_canonical};
use sha2::{Digest, Sha256};
use thiserror::Error;

mod block_scan;
mod change_journal;
pub mod changes;
mod command_transaction;
mod prepared_text_input;
pub use prepared_text_input::{MarkdownPreparedTextInput, MarkdownTextInputComposition};
#[cfg(feature = "s2-table-window-prototype")]
mod cell_draft;
#[cfg(feature = "s2-table-window-prototype")]
pub use cell_draft::{
    MARKDOWN_CELL_DRAFT_COMMIT_UTF16_UNITS, MARKDOWN_CELL_DRAFT_MAX_DELTA_BYTES, MarkdownCellDraft,
    MarkdownCellDraftCommit, MarkdownCellDraftCommitError, MarkdownCellDraftCommitReceipt,
    MarkdownCellDraftCommitResult, MarkdownCellDraftEditCapsule, MarkdownCellDraftError,
    MarkdownCellDraftFacts, MarkdownCellDraftHistoryDirection, MarkdownCellDraftHistoryEdit,
    MarkdownCellDraftHistoryFacts, MarkdownCellDraftInputIntent, MarkdownCellDraftInputKind,
    MarkdownCellDraftPreparedCommit, MarkdownCellDraftReadAuthority, MarkdownCellDraftReadCapsule,
    MarkdownCellDraftResourceObserver, MarkdownCellDraftResourceSnapshot,
    MarkdownCellDraftSourceAuthority, MarkdownCellDraftWordAnchor,
};
mod editing_commands;
mod formatting;
#[cfg(feature = "s2-table-window-prototype")]
mod heading_anchor;
#[cfg(feature = "s2-table-window-prototype")]
pub use heading_anchor::{
    MarkdownHeadingAnchorError, MarkdownHeadingAnchorRequest, MarkdownHeadingAnchorStep,
    MarkdownHeadingAnchorTarget, MarkdownHeadingAnchorWork,
};
mod replacements;
// The native feature exposes bounded table authority. Semantic, geometric, and
// functional completeness remain separate obligations of viewport consumers.
#[cfg(feature = "s2-table-window-prototype")]
mod table_cell_edit;
#[cfg(feature = "s2-table-window-prototype")]
pub use table_cell_edit::MarkdownTableCellEditPlan;
#[cfg(feature = "s2-table-window-prototype")]
mod table_cell_source;
#[cfg(feature = "s2-table-window-prototype")]
pub use table_cell_source::VerifiedMarkdownTableCellSource;
#[cfg(feature = "s2-table-window-prototype")]
mod table_reference_request;
#[cfg(feature = "s2-table-window-prototype")]
mod table_row_append;
#[cfg(feature = "s2-table-window-prototype")]
pub use table_row_append::MarkdownTableRowAppendPlan;
#[cfg(feature = "s2-table-window-prototype")]
mod table_source;
#[cfg(feature = "s2-table-window-prototype")]
mod table_window_request;
#[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
pub use table_window_request::MarkdownSelectionStyleInvariant;
#[cfg(feature = "s2-table-window-prototype")]
pub use table_window_request::{
    MarkdownOrdinaryPendingReason, MarkdownOrdinarySemanticState, MarkdownRootLeafPendingReason,
    MarkdownTableCellEditRequest, MarkdownTableCellEditStep, MarkdownTableCellInlineStep,
    MarkdownTableRequestError, MarkdownTableViewportBudget, MarkdownTableViewportCoverage,
    MarkdownTableViewportRequest, MarkdownTableViewportRequiredRowQuery,
    MarkdownTableViewportRequiredRowResult, MarkdownTableViewportStep, MarkdownTableWindowRequest,
    MarkdownTableWindowStep, MarkdownTableWindowTarget, MarkdownViewportSourceAuthority,
    MarkdownViewportSourceRequest, MarkdownViewportSourceRequiredRowQuery,
    MarkdownViewportSourceRequiredRowResult, MarkdownViewportSourceStep, SemanticWork,
    VerifiedMarkdownOrdinaryLeaf, VerifiedMarkdownOrdinaryLine, VerifiedMarkdownPlainViewport,
    VerifiedMarkdownRootLeafViewport, VerifiedMarkdownTableCellInline, VerifiedMarkdownTableGap,
    VerifiedMarkdownTableRowSource, VerifiedMarkdownTableViewport, VerifiedMarkdownTableWindow,
    VerifiedMarkdownViewportSources,
};

pub use editing_commands::{MarkdownEditingCommand, MarkdownEditingOutcome};

pub use formatting::MarkdownFormatAction;
pub use replacements::{MarkdownReplacement, MarkdownReplacementPart};
mod piece_tree;
#[cfg(feature = "paged-shaping-frontier")]
pub(crate) use piece_tree::ascii_word::{MarkdownAsciiWordRun, MarkdownAsciiWordStep};
// The canonical root stores general Unicode facts only. Each bidi/layout
// algorithm owns the policy that permits its specialized paths.
#[path = "markdown_document_buffer/unicode_properties.rs"]
mod canonical_unicode_properties;
pub(crate) use canonical_unicode_properties::MarkdownUnicodeProperties;
// Frozen text shares immutable read resources without active-document, history,
// or persistence authority.
#[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
pub(crate) mod display_projection;
mod frozen_text;
pub use frozen_text::MarkdownTextSnapshot;
#[cfg(feature = "paged-shaping-frontier")]
pub(crate) use frozen_text::{
    FrozenBidiParagraphCursor, FrozenPrintableAsciiSuffix, FrozenPrintableAsciiSuffixStep,
};
mod queries;
// One derived UTF-16 window accelerates finite reads without owning text or history.
mod read_cache;
mod revision_cache;

use block_scan::*;
use piece_tree::*;

const ORIGINAL_PIECE_BYTES: usize = 4 * 1_024;
const EDIT_SEGMENT_BYTES: usize = 64 * 1_024;
const MAX_EDIT_HISTORY_ENTRIES: usize = 128;
#[cfg(any(test, feature = "native-markdown-candidate"))]
const MAX_MATERIALIZED_UTF8_BYTES: usize = 256 * 1_024;
#[cfg(any(test, feature = "native-markdown-candidate"))]
const MAX_MATERIALIZED_UTF16_UNITS: usize = 128 * 1_024;
#[cfg(any(test, feature = "native-markdown-candidate"))]
const MAX_VIEWPORT_OVERSCAN_UTF16_UNITS: usize = 8 * 1_024;
#[cfg(any(test, feature = "native-markdown-candidate"))]
const MARKDOWN_BLOCK_CHECKPOINT_STRIDE_UTF16_UNITS: usize = 64 * 1_024;

/// A selection uses the native editor's UTF-16 coordinate space.
/// Both positions must be Unicode scalar boundaries before an edit is accepted.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MarkdownTextSelection {
    pub anchor_utf16: usize,
    pub head_utf16: usize,
}

impl MarkdownTextSelection {
    #[must_use]
    /// Construct a collapsed native selection without accessing document storage.
    pub fn caret(utf16_offset: usize) -> Self {
        Self {
            anchor_utf16: utf16_offset,
            head_utf16: utf16_offset,
        }
    }
}

#[cfg(any(test, feature = "native-markdown-candidate"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Observed source line-ending preference without normalizing persisted bytes.
pub enum MarkdownLineEnding {
    LineFeed,
    CarriageReturnLineFeed,
    CarriageReturn,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Content digest used as the optimistic-save baseline.
pub struct MarkdownDocumentRevision([u8; 32]);

impl MarkdownDocumentRevision {
    #[must_use]
    /// Encode a revision using the stable lowercase hexadecimal representation.
    pub fn encode_hex(self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut encoded = String::with_capacity(64);
        for byte in self.0 {
            // Encode both nibbles of each digest byte so revisions always contain exactly 64 ASCII characters.
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        encoded
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Retained storage counters exposed only to regression tests.
pub struct MarkdownDocumentStorageProfile {
    pub mapped_original_bytes: usize,
    pub appended_edit_bytes: usize,
    pub allocated_edit_segment_bytes: usize,
    pub piece_count: usize,
    pub tree_height: usize,
    pub undo_entries: usize,
    pub redo_entries: usize,
}

#[cfg(any(test, feature = "native-markdown-candidate"))]
#[derive(Clone, Debug, Eq, PartialEq)]
/// A bounded owned text slice with its original document UTF-16 coordinates.
pub struct MaterializedMarkdownSlice {
    pub utf16_range: Range<usize>,
    pub text: String,
}

#[cfg(any(test, feature = "native-markdown-candidate"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Whether a bounded slice contains complete Markdown blocks suitable for parsing.
pub enum MarkdownViewportSemanticStatus {
    Complete,
    SourceFallback,
}

#[cfg(any(test, feature = "native-markdown-candidate"))]
#[derive(Clone, Debug, Eq, PartialEq)]
/// A transient bounded viewport and its explicit semantic completeness status.
pub struct MaterializedMarkdownViewport {
    pub utf16_range: Range<usize>,
    pub text: String,
    pub semantic_status: MarkdownViewportSemanticStatus,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Compact replacement geometry sufficient for native history-restoration notifications.
struct MarkdownReplacementMetrics {
    location: usize,
    removed_utf16_length: usize,
    inserted_utf16_length: usize,
    forward_change_in_utf16_length: isize,
}

/// Exact character-edit geometry for a native adapter restoring Rust-owned history.
///
/// The range addresses the document before restoration, so undo/redo notification
/// does not flatten or diff the full document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownHistoryTextEdit {
    pub replaced_utf16_range: Range<usize>,
    pub change_in_utf16_length: isize,
    pub selection_after: MarkdownTextSelection,
}

/// The active Markdown document model.
///
/// Original UTF-8 bytes remain in one session-owned read-only snapshot mapping. Tree leaves refer
/// either to that mapping or to bounded append-only edit segments. There is no
/// API that returns the complete document as a `String`; UI code materializes a
/// bounded viewport and persistence streams leaves directly to a writer.
pub struct MarkdownDocumentBuffer {
    path: PathBuf,
    // Published immutable values may share this independently frozen anonymous
    // inode; active-document destruction cannot revoke their existing read resources.
    original_snapshot: Option<Arc<OriginalBacking>>,
    save_baseline: MarkdownDiskBaseline,
    maximum_document_bytes: usize,
    root: PieceTreeLink,
    initial_root: PieceTreeLink,
    edits: MarkdownEditSegments,
    // Real changes use finite inline records or document-owned anonymous pages,
    // never the UTF-8 text segments themselves.
    change_journal: change_journal::ChangeJournal,
    selection: MarkdownTextSelection,
    // This document exclusively owns its input candidate. Until publication,
    // other writes reject it and readers observe only the canonical root.
    prepared_text_input: Option<prepared_text_input::PendingTextInput>,
    // Describe only the current input composition's history ownership. Its text
    // remains owned by the original root, segments, and final undo entry.
    text_input_composition: Option<prepared_text_input::CompositionGroup>,
    // Permit at most one cell draft per document. Even a retired slot must wait
    // for its actual read roots to return before canonical writes can resume.
    #[cfg(feature = "s2-table-window-prototype")]
    cell_draft: Option<cell_draft::CellDraftSlot>,
    undo: VecDeque<MarkdownEditorMemento>,
    redo: VecDeque<MarkdownEditorMemento>,
    generation: u64,
    // The active document and all independent captures share one derived 8 KiB
    // window, rather than allocating per snapshot. Its root key is weak, so the
    // shared cache Arc cannot keep a document, text allocation, or history alive.
    utf16_read_cache: Arc<Mutex<read_cache::Utf16ReadCache>>,
    // Cache only this immutable root's 32-byte digest; weak identity does not
    // extend an old text/history lifetime.
    revision_cache: Mutex<revision_cache::RevisionCache>,
    #[cfg(any(test, feature = "native-markdown-candidate"))]
    block_checkpoints: RefCell<MarkdownBlockCheckpointIndex>,
}

#[derive(Clone, Copy)]
/// Every opened UTF-8 Markdown model binds the actual disk identity and revision together.
/// Later saves revalidate both rather than inferring overwrite authority from equal live text.
struct MarkdownDiskBaseline {
    identity: MarkdownFileIdentity,
    revision: MarkdownDocumentRevision,
}

impl MarkdownDocumentBuffer {
    /// Open and validate a bounded regular UTF-8 file before publishing session-owned mapped storage.
    pub fn open(
        path: &Path,
        maximum_document_bytes: u64,
    ) -> Result<Self, MarkdownDocumentBufferError> {
        // Validate descriptor acquisition and file extent before indexing or mapping any source bytes.
        let maximum_document_bytes = usize::try_from(maximum_document_bytes).unwrap_or(usize::MAX);
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(|_| {
                // Descriptor or mapping acquisition failed before a usable document could be published.
                MarkdownDocumentBufferError::OpenUnavailable
            })?;
        let before = file.metadata().map_err(|_| {
            // Descriptor or mapping acquisition failed before a usable document could be published.
            MarkdownDocumentBufferError::OpenUnavailable
        })?;
        if !before.is_file() {
            // Reject devices, directories, and other special entries before mapping their bytes.
            return Err(MarkdownDocumentBufferError::NotRegularFile);
        }
        let byte_count = usize::try_from(before.len()).map_err(|_| {
            // The requested extent cannot fit the editor's supported byte or signed-coordinate limit.
            MarkdownDocumentBufferError::DocumentTooLarge
        })?;
        if byte_count > maximum_document_bytes {
            // Enforce the editing size limit before creating a mapping or indexes.
            return Err(MarkdownDocumentBufferError::DocumentTooLarge);
        }
        let original_identity = MarkdownFileIdentity::from(&before);

        // Freeze a distinct unnamed inode before deriving immutable UTF-8 views or tree metadata.
        // External editors may freely replace or truncate the authoritative source after this step.
        let original_snapshot = snapshot_canonical(&file, before.len(), || {
            // Explicit document open is synchronous; its caller has no search cancellation token.
            true
        })
        .map_err(|error| {
            // Snapshot acquisition never exposes unstable source bytes to the mapped document.
            match error {
                // A changed extent rejects the open without publishing a partially indexed document.
                SnapshotError::SourceChanged => {
                    MarkdownDocumentBufferError::DocumentChangedDuringOpen
                }
                // Document open has no cancellation source, but a cancelled acquisition remains unavailable.
                SnapshotError::Cancelled => MarkdownDocumentBufferError::OpenUnavailable,
                // Storage failures release all local private snapshot resources through RAII.
                SnapshotError::Storage(_) => MarkdownDocumentBufferError::OpenUnavailable,
            }
        })?;
        let build = build_original_backing_tree(original_snapshot.as_ref(), 0..byte_count)?;
        let root = build.root;
        let original_revision = build.revision;
        #[cfg(any(test, feature = "native-markdown-candidate"))]
        let block_checkpoints = build.checkpoints;

        // Revalidate both the descriptor and path after indexing so a raced replacement is rejected.
        let after = file.metadata().map_err(|_| {
            // A failed post-index identity inspection cannot establish a stable opened document.
            MarkdownDocumentBufferError::DocumentChangedDuringOpen
        })?;
        let path_after = std::fs::symlink_metadata(path).map_err(|_| {
            // A failed post-index identity inspection cannot establish a stable opened document.
            MarkdownDocumentBufferError::DocumentChangedDuringOpen
        })?;
        if path_after.file_type().is_symlink()
            || !path_after.is_file()
            || MarkdownFileIdentity::from(&after) != original_identity
            || MarkdownFileIdentity::from(&path_after) != original_identity
        {
            // An identity or type change during indexing invalidates the entire open; local resources drop on return.
            return Err(MarkdownDocumentBufferError::DocumentChangedDuringOpen);
        }

        // Transfer all mapping and history ownership into the active document only after validation.
        Ok(Self {
            path: path.to_path_buf(),
            original_snapshot: original_snapshot.map(Arc::new),
            save_baseline: MarkdownDiskBaseline {
                identity: original_identity,
                revision: original_revision,
            },
            maximum_document_bytes,
            initial_root: root.clone(),
            revision_cache: Mutex::new(revision_cache::RevisionCache::seeded(
                &root,
                original_revision,
            )),
            root,
            edits: MarkdownEditSegments::default(),
            change_journal: change_journal::ChangeJournal::default(),
            selection: MarkdownTextSelection::default(),
            prepared_text_input: None,
            text_input_composition: None,
            #[cfg(feature = "s2-table-window-prototype")]
            cell_draft: None,
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            generation: 0,
            utf16_read_cache: Arc::new(Mutex::new(read_cache::Utf16ReadCache::default())),
            #[cfg(any(test, feature = "native-markdown-candidate"))]
            block_checkpoints: RefCell::new(block_checkpoints),
        })
    }

    #[must_use]
    /// Borrow the current authoritative path without transferring document ownership.
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    /// Return the verified revision acquired by opening the file or completing a validated save.
    /// Live edits do not change this disk baseline until persistence actually succeeds.
    pub fn original_revision(&self) -> MarkdownDocumentRevision {
        self.save_baseline.revision
    }

    /// Computes the current edited revision without materializing a flat
    /// document. This full traversal is reserved for explicit persistence and
    /// conflict recovery boundaries; it is never part of the input hot path.
    /// Hash live piece bytes only at an explicit persistence boundary.
    pub fn current_revision(
        &self,
    ) -> Result<MarkdownDocumentRevision, MarkdownDocumentBufferError> {
        self.original_backing_health()?;
        let mut cache = match self.revision_cache.lock() {
            // A valid lock protects only this original root's derived digest.
            Ok(cache) => cache,
            Err(_) => {
                // A synchronization failure cannot revive cached authority.
                // Preserve any real shared-backing failure before reporting poison.
                self.original_backing_health()?;
                return Err(MarkdownDocumentBufferError::RevisionCacheUnavailable);
            }
        };
        cache.read(self, &self.root)
    }

    #[must_use]
    /// Conservatively detect path replacement or metadata changes before attempting a save.
    pub fn original_file_identity_is_unchanged(&self) -> bool {
        let identity = self.save_baseline.identity;
        let Ok(metadata) = std::fs::symlink_metadata(&self.path) else {
            // An unreadable or missing path cannot establish that the opened file is unchanged.
            return false;
        };
        !metadata.file_type().is_symlink()
            && metadata.is_file()
            && MarkdownFileIdentity::from(&metadata) == identity
    }

    /// Rebinds a freshly opened save mapping after its sibling temporary file
    /// has been atomically renamed over the authoritative note.
    ///
    /// The mapping remains valid across `rename(2)`, but its stored path must
    /// change from the temporary sibling to the canonical vault destination.
    /// Identity is revalidated before the path is accepted so a concurrent
    /// replacement cannot silently attach this session to another file.
    /// Rebind an already-opened save mapping only after validating the renamed file identity.
    pub(crate) fn adopt_atomic_save_destination(
        &mut self,
        destination: &Path,
    ) -> Result<(), MarkdownDocumentBufferError> {
        self.ensure_text_input_idle()?;
        let identity = self.save_baseline.identity;
        let metadata = std::fs::symlink_metadata(destination).map_err(|_| {
            // A failed destination identity inspection cannot establish ownership of the newly saved file.
            MarkdownDocumentBufferError::DocumentChangedDuringSave
        })?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || MarkdownFileIdentity::from(&metadata) != identity
        {
            // A replaced destination cannot be adopted as if it were the newly saved mapping.
            return Err(MarkdownDocumentBufferError::DocumentChangedDuringSave);
        }
        self.path = destination.to_path_buf();
        Ok(())
    }

    /// Accept a verified identical saved file without replacing canonical storage or editor history.
    ///
    /// The validation buffer owns a private immutable snapshot of the published file. Its path and
    /// identity must still match this document, and its digest must match the live pieces. The saved
    /// root changes the dirty comparison only; undo/redo, selection, generation, mapping and edit
    /// segments remain untouched so both explicit save and autosave preserve the user's history.
    pub(crate) fn adopt_saved_baseline(
        &mut self,
        saved: &Self,
    ) -> Result<(), MarkdownDocumentBufferError> {
        self.ensure_text_input_idle()?;
        // This saved model was opened from a real file. Its mandatory baseline still requires exact
        // path, unchanged file identity, and current piece-revision agreement before adoption.
        let saved_revision = saved.original_revision();
        if self.path != saved.path
            || !saved.original_file_identity_is_unchanged()
            || self.current_revision()? != saved_revision
        {
            // A raced destination or different content cannot authorize changing this draft's save baseline.
            return Err(MarkdownDocumentBufferError::DocumentChangedDuringSave);
        }
        // Only validated metadata and one shared root become the new conflict/dirty baseline.
        self.save_baseline = saved.save_baseline;
        self.initial_root = self.root.clone();
        Ok(())
    }

    #[must_use]
    /// Read the exact live byte count from the root aggregate in constant time.
    pub fn byte_len(&self) -> usize {
        piece_tree_metrics(&self.root).byte_len
    }

    #[must_use]
    /// Read the native UTF-16 coordinate length without materializing UTF-16 storage.
    pub fn utf16_len(&self) -> usize {
        piece_tree_metrics(&self.root).utf16_len
    }

    #[must_use]
    /// Freeze the current canonical text by copying only resource references and
    /// sparse source metadata, never body bytes or active-document ownership.
    ///
    /// Prepared but unpublished input is absent from this root. Later edits,
    /// undo/redo, path changes, and document destruction cannot change the value.
    /// Its referenced anonymous backings/segments end with the final snapshot owner.
    pub fn text_snapshot(&self) -> MarkdownTextSnapshot {
        MarkdownTextSnapshot::capture(self)
    }

    #[must_use]
    #[cfg(any(test, feature = "native-markdown-candidate"))]
    /// Count logical lines, including the empty final line after a trailing line break.
    pub fn line_count(&self) -> usize {
        piece_tree_metrics(&self.root).line_breaks.saturating_add(1)
    }

    #[must_use]
    #[cfg(any(test, feature = "native-markdown-candidate"))]
    /// Choose the observed document line-ending preference from aggregate flags.
    pub fn line_ending(&self) -> MarkdownLineEnding {
        let metrics = piece_tree_metrics(&self.root);
        if metrics.contains_crlf {
            // CRLF takes precedence when present, preserving the existing mixed-ending preference.
            MarkdownLineEnding::CarriageReturnLineFeed
        } else if metrics.contains_carriage_return {
            // Without CRLF, an observed CR selects the legacy carriage-return style.
            MarkdownLineEnding::CarriageReturn
        } else {
            // Documents with LF only, or no line endings, use line feed.
            MarkdownLineEnding::LineFeed
        }
    }

    #[must_use]
    #[cfg(any(test, feature = "native-markdown-candidate"))]
    /// Expose the edit generation used to invalidate viewport presentation caches.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    /// Read the current model selection in UTF-16 coordinates.
    pub fn selection(&self) -> MarkdownTextSelection {
        self.selection
    }

    /// Validate both scalar boundaries before replacing the model selection.
    pub fn set_selection(
        &mut self,
        selection: MarkdownTextSelection,
    ) -> Result<(), MarkdownDocumentBufferError> {
        self.ensure_text_input_idle()?;
        self.validate_selection(selection)?;
        self.selection = selection;
        // An explicit programmatic selection breaks composition. Later input at
        // the same position must not be merged into that old history group.
        self.text_input_composition = None;
        Ok(())
    }

    #[must_use]
    /// Compare live and saved roots without hashing on the input path; undo to the saved root is clean.
    pub fn is_dirty(&self) -> bool {
        !piece_tree_links_share_root(&self.root, &self.initial_root)
    }

    #[must_use]
    #[cfg(any(test, feature = "native-markdown-candidate"))]
    /// Report whether a bounded prior editor state is available.
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    #[must_use]
    #[cfg(any(test, feature = "native-markdown-candidate"))]
    /// Report whether an undone editor state can be restored.
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    #[cfg(any(test, feature = "native-markdown-candidate"))]
    /// Apply a scalar-aligned replacement using the model selection as its undo origin.
    pub fn replace_utf16_range(
        &mut self,
        range: Range<usize>,
        replacement: &str,
        selection_after: MarkdownTextSelection,
    ) -> Result<(), MarkdownDocumentBufferError> {
        self.replace_utf16_range_with_selection(range, replacement, self.selection, selection_after)
    }

    /// Preserve real change recording for the convenience entry point; callers
    /// needing no derived output use the existing no-op observer.
    pub fn replace_utf16_range_with_selection(
        &mut self,
        range: Range<usize>,
        replacement: &str,
        selection_before: MarkdownTextSelection,
        selection_after: MarkdownTextSelection,
    ) -> Result<(), MarkdownDocumentBufferError> {
        self.replace_utf16_range_with_selection_observed(
            range,
            replacement,
            selection_before,
            selection_after,
            changes::NoopObserver,
        )
        .map(|_| ())
        .map_err(changes::legacy_error)
    }

    /// Stream actual old-coordinate changes before commit. Observer failure
    /// preserves the original root, selection, append budget, undo, and redo.
    pub fn replace_utf16_range_with_selection_observed<O: changes::MarkdownTextChangeObserver>(
        &mut self,
        range: Range<usize>,
        replacement: &str,
        selection_before: MarkdownTextSelection,
        selection_after: MarkdownTextSelection,
        observer: O,
    ) -> Result<O::Output, changes::MarkdownObservedEditError<O::Error>> {
        // The single-stage convenience entry reuses the same prepare/publish
        // transaction; preparation failure still precedes any canonical switch.
        let (token, output) = self.prepare_text_input_observed(
            range,
            replacement,
            selection_before,
            selection_after,
            observer,
        )?;
        self.publish_text_input(&token)?;
        Ok(output)
    }

    #[cfg(any(test, feature = "native-markdown-candidate"))]
    /// Insert text through the same validated replacement transaction as ordinary editing.
    pub fn insert_utf16(
        &mut self,
        utf16_offset: usize,
        text: &str,
        selection_after: MarkdownTextSelection,
    ) -> Result<(), MarkdownDocumentBufferError> {
        self.replace_utf16_range(utf16_offset..utf16_offset, text, selection_after)
    }

    #[cfg(any(test, feature = "native-markdown-candidate"))]
    /// Delete a range through the shared replacement and history path.
    pub fn delete_utf16_range(
        &mut self,
        range: Range<usize>,
        selection_after: MarkdownTextSelection,
    ) -> Result<(), MarkdownDocumentBufferError> {
        self.replace_utf16_range(range, "", selection_after)
    }

    #[cfg(any(test, feature = "native-markdown-candidate"))]
    /// Restore the previous editor state and expose its selection to the native caller.
    pub fn undo(&mut self) -> Option<MarkdownTextSelection> {
        self.undo_text_edit().map(|edit| {
            // Expose only the restored selection through the convenience API; exact edit geometry remains available separately.
            edit.selection_after
        })
    }

    /// The selection-only convenience API leaves history unchanged on journal
    /// corruption/I/O failure; callers needing typed failure use the observed API.
    pub fn undo_text_edit(&mut self) -> Option<MarkdownHistoryTextEdit> {
        self.undo_text_edit_observed(changes::NoopObserver)
            .ok()
            .flatten()
            .map(|(edit, ())| edit)
    }

    /// Peek inverse changes and complete derived state before moving the original
    /// root and unique journal ticket; failures do not pop history.
    pub fn undo_text_edit_observed<O: changes::MarkdownTextChangeObserver>(
        &mut self,
        observer: O,
    ) -> changes::MarkdownObservedHistoryResult<O> {
        self.restore_history_observed(true, observer)
    }

    #[cfg(any(test, feature = "native-markdown-candidate"))]
    /// Return only the restored selection while preserving state on journal-read failure.
    pub fn redo(&mut self) -> Option<MarkdownTextSelection> {
        // Exact change geometry and typed errors remain available at the observed boundary.
        self.redo_text_edit().map(|edit| edit.selection_after)
    }

    /// The convenience API cannot report a failed read as completed redo;
    /// preserve original history and text for a later explicit attempt.
    pub fn redo_text_edit(&mut self) -> Option<MarkdownHistoryTextEdit> {
        self.redo_text_edit_observed(changes::NoopObserver)
            .ok()
            .flatten()
            .map(|(edit, ())| edit)
    }

    /// Complete forward-change replay before popping history. Every change
    /// retains the original transaction's old-coordinate interpretation.
    pub fn redo_text_edit_observed<O: changes::MarkdownTextChangeObserver>(
        &mut self,
        observer: O,
    ) -> changes::MarkdownObservedHistoryResult<O> {
        self.restore_history_observed(false, observer)
    }

    /// Prepare the observer's complete result from the original history entry,
    /// then transfer that one entry and its journal ownership without copying pages.
    fn restore_history_observed<O: changes::MarkdownTextChangeObserver>(
        &mut self,
        inverse: bool,
        mut observer: O,
    ) -> changes::MarkdownObservedHistoryResult<O> {
        self.ensure_text_input_idle()?;
        let source = if inverse { &self.undo } else { &self.redo };
        let Some(target) = source.back() else {
            // No history means no observer callback or manufactured character change.
            return Ok(None);
        };
        self.change_journal
            .visit(&target.changes, inverse, |record| {
                // Journal records already carry this restoration direction's
                // exact old extent and inserted length; do not infer a whole-text diff.
                observer.changed(changes::MarkdownTextChange {
                    old_utf16_range: record.start..record.start + record.removed,
                    inserted_utf16_len: record.inserted,
                })
            })
            .map_err(|error| match error {
                // Journal read/validation failure ends the peek phase before
                // history movement; the observer's partial candidate is discarded.
                change_journal::VisitError::Journal(error) => {
                    changes::MarkdownObservedEditError::Journal(error)
                }
                change_journal::VisitError::Observer(error) => {
                    // Keep the observer's own typed error rather than relabeling
                    // it as storage failure or committing a partial derived value.
                    changes::MarkdownObservedEditError::Observer(error)
                }
            })?;
        let output = observer
            .finish(
                changes::MarkdownPreparedText {
                    document: self,
                    root: &target.root,
                },
                target.selection,
            )
            .map_err(changes::MarkdownObservedEditError::Observer)?;
        // All fallible observer/journal work has ended. Transfer the unique
        // ticket to the opposite history side without copying its anonymous pages.
        let target = if inverse {
            self.undo.pop_back()
        } else {
            self.redo.pop_back()
        }
        .expect("the immutable peek validated one existing history entry");
        let opposite = MarkdownEditorMemento {
            // Composition stores its actual counterpart selection; ordinary
            // entries retain the established exchange with the current selection.
            root: self.root.clone(),
            selection: target.counterpart_selection.unwrap_or(self.selection),
            counterpart_selection: target.counterpart_selection.map(|_| target.selection),
            replacement: target.replacement,
            changes: target.changes,
        };
        let evicted = if inverse {
            push_bounded_snapshot(&mut self.redo, opposite)
        } else {
            push_bounded_snapshot(&mut self.undo, opposite)
        };
        if let Some(evicted) = evicted {
            // Only the newly committed bounded-history eviction releases its
            // journal ownership, after the replacement entry has been installed.
            self.change_journal.release(evicted.changes);
        }
        self.root = target.root;
        self.selection = target.selection;
        // Completed history restoration ends composition; a failed earlier peek
        // leaves that original grouping untouched.
        self.text_input_composition = None;
        self.generation = self.generation.wrapping_add(1);
        #[cfg(any(test, feature = "native-markdown-candidate"))]
        self.block_checkpoints
            .get_mut()
            .invalidate_after(target.replacement.location);
        let edit = if inverse {
            target.replacement.undo_text_edit(self.selection)
        } else {
            target.replacement.redo_text_edit(self.selection)
        };
        Ok(Some((edit, output)))
    }

    /// Translate a validated UTF-16 boundary into a UTF-8 byte position.
    pub fn byte_offset_for_utf16(
        &self,
        utf16_offset: usize,
    ) -> Result<usize, MarkdownDocumentBufferError> {
        if utf16_offset > self.utf16_len() {
            // Positions beyond the document are rejected before descending the tree.
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        byte_offset_for_utf16_in_tree(self, self.root.as_deref(), utf16_offset, 0)
    }

    /// Translate a validated byte boundary into a UTF-16 position.
    pub fn utf16_offset_for_byte(
        &self,
        byte_offset: usize,
    ) -> Result<usize, MarkdownDocumentBufferError> {
        if byte_offset > self.byte_len() {
            // Reject oversized byte offsets before the leaf decoder can slice them.
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        utf16_offset_for_byte_in_tree(self, self.root.as_deref(), byte_offset, 0)
    }

    /// Read an NSString-compatible unit through one at-most-4096-unit derived
    /// window, without constructing a whole-document encoding mirror.
    pub fn utf16_code_unit_at(
        &self,
        utf16_offset: usize,
    ) -> Result<u16, MarkdownDocumentBufferError> {
        // Source failure precedes cache hits. EOF has no readable unit, and old
        // cached bytes cannot turn an unavailable source into a successful read.
        self.original_backing_health()?;
        if utf16_offset >= self.utf16_len() {
            // Reject EOF or outside unit positions before requesting cache contents.
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        let mut unit = [0];
        self.copy_utf16_code_units(utf16_offset, &mut unit)?;
        Ok(unit[0])
    }

    /// Fill the caller's complete unit range, including a low-surrogate start,
    /// while retaining only one internal 8 KiB cache window at a time.
    ///
    /// Larger explicit requests proceed window by window over their exact range,
    /// without truncating native unit semantics. Root and generation both bind
    /// cache hits; edits, undo, and temporarily switched candidate roots cannot reuse an old key.
    pub fn copy_utf16_code_units(
        &self,
        utf16_offset: usize,
        destination: &mut [u16],
    ) -> Result<(), MarkdownDocumentBufferError> {
        self.original_backing_health()?;
        let mut cache = self
            .utf16_read_cache
            .lock()
            .map_err(|_| MarkdownDocumentBufferError::Utf16ReadCacheUnavailable)?;
        // Hold the lock only for Rust reads. This cache contains neither native
        // calls nor a hidden strong link back to an active document.
        cache.copy(self, &self.root, self.generation, utf16_offset, destination)
    }

    #[cfg(any(test, feature = "native-markdown-candidate"))]
    /// Locate a logical line start using cached break counts and CRLF-aware descent.
    pub fn byte_offset_for_line(
        &self,
        zero_based_line: usize,
    ) -> Result<usize, MarkdownDocumentBufferError> {
        self.original_backing_health()?;
        if zero_based_line == 0 {
            // The first line always starts at byte zero, including an empty document.
            return Ok(0);
        }
        let break_index = zero_based_line - 1;
        if break_index >= piece_tree_metrics(&self.root).line_breaks {
            // A requested line needs a preceding logical break within the document.
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        find_line_break_end(
            self,
            self.root.as_ref().expect("non-empty line break tree"),
            break_index,
            0,
        )
    }

    #[cfg(any(test, feature = "native-markdown-candidate"))]
    /// Find the zero-based logical line containing a scalar-aligned byte boundary.
    pub fn line_for_byte(&self, byte_offset: usize) -> Result<usize, MarkdownDocumentBufferError> {
        if byte_offset > self.byte_len() {
            // Reject positions beyond the live byte length before prefix aggregation.
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        Ok(metrics_for_tree_prefix(self, self.root.as_deref(), byte_offset)?.line_breaks)
    }

    #[cfg(any(test, feature = "native-markdown-candidate"))]
    /// Copy only a bounded, scalar-aligned viewport and caller-requested overscan.
    pub fn materialize_viewport(
        &self,
        requested_utf16_range: Range<usize>,
        overscan_utf16_units: usize,
    ) -> Result<MaterializedMarkdownSlice, MarkdownDocumentBufferError> {
        self.original_backing_health()?;
        if requested_utf16_range.start > requested_utf16_range.end
            || requested_utf16_range.end > self.utf16_len()
        {
            // Invalid or reversed requested ranges cannot define a view of the document.
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        if overscan_utf16_units > MAX_VIEWPORT_OVERSCAN_UTF16_UNITS {
            // Bound overscan independently so callers cannot bypass materialization limits.
            return Err(MarkdownDocumentBufferError::MaterializationLimitExceeded);
        }
        self.byte_offset_for_utf16(requested_utf16_range.start)?;
        self.byte_offset_for_utf16(requested_utf16_range.end)?;
        let mut start = requested_utf16_range
            .start
            .saturating_sub(overscan_utf16_units);
        while start > 0
            && self.byte_offset_for_utf16(start)
                == Err(MarkdownDocumentBufferError::NotUnicodeBoundary)
        {
            // Retreat an overscan start inside a surrogate pair to include the complete scalar.
            start -= 1;
        }
        let mut end = requested_utf16_range
            .end
            .saturating_add(overscan_utf16_units)
            .min(self.utf16_len());
        while end < self.utf16_len()
            && self.byte_offset_for_utf16(end)
                == Err(MarkdownDocumentBufferError::NotUnicodeBoundary)
        {
            // Advance an overscan end inside a surrogate pair to include the complete scalar.
            end += 1;
        }
        if end - start > MAX_MATERIALIZED_UTF16_UNITS {
            // Reject an expanded UTF-16 range exceeding the fixed viewport budget.
            return Err(MarkdownDocumentBufferError::MaterializationLimitExceeded);
        }
        let byte_start = self.byte_offset_for_utf16(start)?;
        let byte_end = self.byte_offset_for_utf16(end)?;
        if byte_end - byte_start > MAX_MATERIALIZED_UTF8_BYTES {
            // Enforce a separate byte budget for multibyte text before allocating its String.
            return Err(MarkdownDocumentBufferError::MaterializationLimitExceeded);
        }
        // Allocate only the validated viewport byte extent and append scalar-aligned leaf overlaps.
        let mut text = String::with_capacity(byte_end - byte_start);
        visit_piece_tree_range(
            self,
            self.root.as_deref(),
            byte_start..byte_end,
            0,
            &mut |bytes| {
                // Every piece and requested boundary is an UTF-8 scalar boundary.
                text.push_str(std::str::from_utf8(bytes).expect("validated piece bytes"));
                Ok(())
            },
        )?;
        Ok(MaterializedMarkdownSlice {
            utf16_range: start..end,
            text,
        })
    }

    /// Materialize a complete bounded block envelope for Markdown parsing.
    ///
    /// Checkpoints retain line boundaries and compact scanner state, not text or
    /// ASTs. Oversized fences/tables/blocks explicitly return source fallback;
    /// callers must not parse that partial slice as an independent document.
    #[cfg(any(test, feature = "native-markdown-candidate"))]
    /// Expand a viewport to complete bounded Markdown blocks, or explicitly return source fallback.
    pub fn materialize_markdown_viewport(
        &self,
        requested_utf16_range: Range<usize>,
        overscan_utf16_units: usize,
    ) -> Result<MaterializedMarkdownViewport, MarkdownDocumentBufferError> {
        self.original_backing_health()?;
        if requested_utf16_range.start > requested_utf16_range.end
            || requested_utf16_range.end > self.utf16_len()
        {
            // Reject invalid requested coordinates before accessing checkpoint state.
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        if overscan_utf16_units > MAX_VIEWPORT_OVERSCAN_UTF16_UNITS {
            // Overscan must remain within the independent viewport bound.
            return Err(MarkdownDocumentBufferError::MaterializationLimitExceeded);
        }
        if self.utf16_len() == 0 {
            // An empty document is already a complete semantic envelope and requires no scan.
            return Ok(MaterializedMarkdownViewport {
                utf16_range: 0..0,
                text: String::new(),
                semantic_status: MarkdownViewportSemanticStatus::Complete,
            });
        }

        // Restart from sparse state before the desired view; a known active block requires its real start.
        let desired_start = requested_utf16_range
            .start
            .saturating_sub(overscan_utf16_units);
        let desired_end = requested_utf16_range
            .end
            .saturating_add(overscan_utf16_units)
            .min(self.utf16_len());
        let checkpoint = self
            .block_checkpoints
            .borrow()
            .checkpoint_at_or_before(desired_start);
        let (scan_start, scan_byte_start, initial_state) = checkpoint.state.active.map_or_else(
            || {
                // A boundary without an active block can resume directly from the checkpoint.
                (
                    checkpoint.utf16_offset,
                    checkpoint.byte_offset,
                    checkpoint.state,
                )
            },
            |active| {
                // Rewind to the active block start so partial fences and tables are never parsed independently.
                (
                    active.start_utf16,
                    active.start_byte,
                    MarkdownBlockScannerState::at_block_start(
                        active.start_utf16,
                        active.start_byte,
                    ),
                )
            },
        );
        if desired_end.saturating_sub(scan_start) > MAX_MATERIALIZED_UTF16_UNITS {
            // An active block starting too far before the view cannot fit within the semantic budget.
            return self
                .materialize_markdown_source_fallback(requested_utf16_range, overscan_utf16_units);
        }

        // Bound semantic scanning independently in UTF-8 bytes and UTF-16 units, then align its endpoint.
        let maximum_byte_end = scan_byte_start
            .saturating_add(MAX_MATERIALIZED_UTF8_BYTES)
            .min(self.byte_len());
        let maximum_byte_end = self.scalar_aligned_byte_offset_down(maximum_byte_end)?;
        let maximum_utf16_by_bytes = self.utf16_offset_for_byte(maximum_byte_end)?;
        let scan_end = scan_start
            .saturating_add(MAX_MATERIALIZED_UTF16_UNITS)
            .min(maximum_utf16_by_bytes)
            .min(self.utf16_len());
        if scan_end < desired_end {
            // The byte cap can be tighter than the UTF-16 cap; retain source fallback when the desired view cannot fit.
            return self
                .materialize_markdown_source_fallback(requested_utf16_range, overscan_utf16_units);
        }

        if self.line_count() == 1
            && scan_start == 0
            && scan_byte_start == 0
            && scan_end < self.utf16_len()
            && desired_end > 0
        {
            // The scanner consumes complete physical CR/LF lines. If a budget
            // cuts the sole line before actual EOF, it stops before consume_line,
            // yielding neither a complete block nor a new checkpoint. Use the
            // existing explicit source status without allocating a doomed scan String.
            // A desired_end=0 request keeps the complete-empty path below.
            // Preserve the ordinary slice's split-surrogate error at its UTF-16 cap.
            self.byte_offset_for_utf16(scan_end)?;
            return self
                .materialize_markdown_source_fallback(requested_utf16_range, overscan_utf16_units);
        }

        // The temporary scan text is bounded; retain only new sparse checkpoints after scanning.
        let scan_slice = self.materialize_viewport(scan_start..scan_end, 0)?;
        let scan = scan_markdown_block_slice(
            &scan_slice.text,
            scan_start,
            scan_byte_start,
            initial_state,
            scan_end == self.utf16_len(),
            true,
        );
        self.block_checkpoints
            .borrow_mut()
            .extend_from_scan(&scan.checkpoints);

        if desired_end > scan.scanned_utf16_end {
            // A bounded scan that did not reach the desired end cannot support complete Markdown semantics.
            return self
                .materialize_markdown_source_fallback(requested_utf16_range, overscan_utf16_units);
        }

        // Expand to complete intersecting blocks and reject unfinished or oversized envelopes below.
        let mut envelope_start = desired_start;
        let mut envelope_end = desired_end;
        for block in &scan.completed_blocks {
            // Expand the envelope only for completed blocks intersecting the desired view.
            if markdown_utf16_ranges_intersect(&block.utf16_range, &(desired_start..desired_end)) {
                // An intersecting complete block must be parsed from its actual start through its actual end.
                envelope_start = envelope_start.min(block.utf16_range.start);
                envelope_end = envelope_end.max(block.utf16_range.end);
            }
        }
        if let Some(active) = scan.final_state.active
            && active.start_utf16 < desired_end
            && scan.scanned_utf16_end > desired_start
        {
            // An unfinished intersecting block has no safe complete envelope inside this scan.
            return self
                .materialize_markdown_source_fallback(requested_utf16_range, overscan_utf16_units);
        }

        envelope_start =
            markdown_utf16_boundary_down_in_text(&scan_slice.text, scan_start, envelope_start)?;
        envelope_end =
            markdown_utf16_boundary_up_in_text(&scan_slice.text, scan_start, envelope_end)?;
        if envelope_start < scan_start
            || envelope_end > scan.scanned_utf16_end
            || envelope_end.saturating_sub(envelope_start) > MAX_MATERIALIZED_UTF16_UNITS
        {
            // Boundary alignment must not escape the scanned or materialized envelope.
            return self
                .materialize_markdown_source_fallback(requested_utf16_range, overscan_utf16_units);
        }

        // Copy only the complete selected envelope; the wider scan String drops when this call returns.
        let local_byte_start = byte_offset_for_utf16_text(
            &scan_slice.text,
            envelope_start.saturating_sub(scan_start),
        )?;
        let local_byte_end =
            byte_offset_for_utf16_text(&scan_slice.text, envelope_end.saturating_sub(scan_start))?;
        let text = scan_slice
            .text
            .get(local_byte_start..local_byte_end)
            .ok_or(MarkdownDocumentBufferError::NotUnicodeBoundary)?
            .to_owned();
        Ok(MaterializedMarkdownViewport {
            utf16_range: envelope_start..envelope_end,
            text,
            semantic_status: MarkdownViewportSemanticStatus::Complete,
        })
    }

    /// Stream live pieces to the caller's writer while preserving exact stored bytes.
    pub fn write_to(
        &self,
        destination: &mut impl Write,
    ) -> Result<(), MarkdownDocumentBufferError> {
        write_piece_tree(self, self.root.as_deref(), destination)
    }

    /// Stream an actual UTF-16 selection as original UTF-8 bytes without a body
    /// String or separate selection-content mirror.
    ///
    /// Both endpoints must be scalar boundaries before the writer is called.
    /// Empty ranges/EOF also check original source health. Write failure may leave
    /// partial caller-owned output without changing canonical text/selection/history.
    /// Actual lease reclamation currently takes precedence over writer failure;
    /// the destination owner must not treat incomplete output as successful copying.
    pub fn write_utf16_range_to(
        &self,
        range: Range<usize>,
        destination: &mut impl Write,
    ) -> Result<(), MarkdownDocumentBufferError> {
        self.original_backing_health()?;
        if range.start > range.end || range.end > self.utf16_len() {
            // Reject reversed/outside coordinates rather than clamping them and
            // writing a different interval than the caller selected.
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        let start = self.byte_offset_for_utf16(range.start)?;
        let end = self.byte_offset_for_utf16(range.end)?;
        // Both conversions exclude surrogate interiors first. Then borrow only
        // intersecting finite leaves and preserve their exact UTF-8 representation.
        visit_piece_tree_range(self, self.root.as_deref(), start..end, 0, &mut |bytes| {
            // The caller owns partial-output rollback. This source lease must
            // actually finish before its writer result can propagate.
            destination
                .write_all(bytes)
                .map_err(|_| MarkdownDocumentBufferError::WriteUnavailable)
        })
    }

    #[must_use]
    #[cfg(test)]
    /// Measure retained mapping, edit, tree, and history state for storage regression checks.
    pub fn storage_profile(&self) -> MarkdownDocumentStorageProfile {
        MarkdownDocumentStorageProfile {
            mapped_original_bytes: self.original_snapshot.as_ref().map_or(0, |mapping| {
                // Measure the mapping extent without touching or copying its backing pages.
                mapping.len()
            }),
            appended_edit_bytes: self.edits.used_bytes,
            allocated_edit_segment_bytes: self.edits.allocated_bytes(),
            piece_count: piece_tree_leaf_count(self.root.as_deref()),
            tree_height: usize::from(piece_tree_height(&self.root)),
            undo_entries: self.undo.len(),
            redo_entries: self.redo.len(),
        }
    }

    #[cfg(any(test, feature = "native-markdown-candidate"))]
    /// Publish a bounded source slice explicitly marked unsuitable for independent semantic parsing.
    fn materialize_markdown_source_fallback(
        &self,
        requested_utf16_range: Range<usize>,
        overscan_utf16_units: usize,
    ) -> Result<MaterializedMarkdownViewport, MarkdownDocumentBufferError> {
        let slice = self.materialize_viewport(requested_utf16_range, overscan_utf16_units)?;
        Ok(MaterializedMarkdownViewport {
            utf16_range: slice.utf16_range,
            text: slice.text,
            semantic_status: MarkdownViewportSemanticStatus::SourceFallback,
        })
    }

    #[cfg(any(test, feature = "native-markdown-candidate"))]
    /// Retreat a byte cap to the nearest valid scalar boundary without changing other errors.
    fn scalar_aligned_byte_offset_down(
        &self,
        mut byte_offset: usize,
    ) -> Result<usize, MarkdownDocumentBufferError> {
        loop {
            // At most the remaining continuation bytes of one scalar need to be skipped.
            match self.utf16_offset_for_byte(byte_offset) {
                // A validated boundary is the final aligned cap.
                Ok(_) => return Ok(byte_offset),
                // A UTF-8 continuation position is corrected by retreating one byte.
                Err(MarkdownDocumentBufferError::NotUnicodeBoundary) if byte_offset > 0 => {
                    byte_offset -= 1;
                }
                // Range and storage failures are unrelated to alignment and propagate to the caller.
                Err(error) => return Err(error),
            }
        }
    }

    /// Validate both the extent and scalar alignment of a current-document selection.
    fn validate_selection(
        &self,
        selection: MarkdownTextSelection,
    ) -> Result<(), MarkdownDocumentBufferError> {
        self.validate_selection_for_result(selection, self.utf16_len())?;
        self.byte_offset_for_utf16(selection.anchor_utf16)?;
        self.byte_offset_for_utf16(selection.head_utf16)?;
        Ok(())
    }

    /// Check selection extents against a prospective document before mutating it.
    fn validate_selection_for_result(
        &self,
        selection: MarkdownTextSelection,
        result_utf16_len: usize,
    ) -> Result<(), MarkdownDocumentBufferError> {
        if selection.anchor_utf16 > result_utf16_len || selection.head_utf16 > result_utf16_len {
            // Either endpoint outside the future document rejects the whole edit transaction.
            Err(MarkdownDocumentBufferError::RangeOutsideDocument)
        } else {
            // Both endpoints fit; scalar alignment is checked against the appropriate source separately.
            Ok(())
        }
    }

    /// Validate a future selection against unchanged prefix, replacement, or translated suffix coordinates.
    fn validate_post_edit_utf16_boundary(
        &self,
        result_offset: usize,
        replaced_range: &Range<usize>,
        replacement: &str,
        replacement_utf16_len: usize,
    ) -> Result<(), MarkdownDocumentBufferError> {
        let inserted_end = replaced_range.start + replacement_utf16_len;
        if result_offset < replaced_range.start {
            // A prefix selection retains its original document coordinate.
            self.byte_offset_for_utf16(result_offset)?;
        } else if result_offset <= inserted_end {
            // A selection inside the inserted region must be a boundary in the replacement string.
            byte_offset_for_utf16_text(replacement, result_offset - replaced_range.start)?;
        } else {
            // A suffix selection maps back across the replaced interval into the original document.
            self.byte_offset_for_utf16(replaced_range.end + result_offset - inserted_end)?;
        }
        Ok(())
    }

    /// Arm the existing reclaim-failure injection for this real original backing;
    /// setting the flag alone does not prove any kernel resource operation failed.
    #[cfg(test)]
    pub(crate) fn fail_next_original_reclaim_for_test(&self) {
        if let Some(backing) = &self.original_snapshot {
            backing.fail_next_reclaim_for_test();
        }
    }

    #[cfg(test)]
    /// Explicitly perform real unpinned-page eviction for regression tests;
    /// an injected flag alone is not evidence of completed or failed I/O.
    pub(crate) fn reclaim_original_read_windows_for_test(
        &self,
    ) -> Result<(), MarkdownDocumentBufferError> {
        match self.original_snapshot.as_ref() {
            // Use the actual original backing's reclamation boundary when one exists.
            Some(backing) => backing.reclaim_unpinned_for_test().map_err(Into::into),
            // An empty original has no original mapping pages to evict.
            None => Ok(()),
        }
    }

    /// A recorded reclamation failure permanently revokes later reads/commits;
    /// a metadata length grants no independent authority to access content.
    pub(crate) fn original_backing_health(&self) -> Result<(), MarkdownDocumentBufferError> {
        if let Some(backing) = self.original_snapshot.as_deref() {
            // Original-source failure precedes later edit-file failure; neither
            // may be bypassed by a metadata-only query that needs source authority.
            backing.health()?;
        }
        self.edits.health()
    }

    /// Observe actual final VM release through scalar facts without retaining
    /// the original text allocation or backing resource.
    #[must_use]
    pub fn original_backing_observer(
        &self,
    ) -> Option<crate::original_backing::MarkdownOriginalBackingObserver> {
        self.original_snapshot
            .as_ref()
            .map(|backing| backing.observer())
    }

    /// Observe the document's original backing and actually published file
    /// segments without retaining their content, descriptors, or mappings.
    ///
    /// History-only published segments still belong to this document's release
    /// obligation; unpublished candidates belong to their original transaction.
    /// Acquire observations before destruction and inspect VM facts only after
    /// the real final resource owner has ended.
    pub fn backing_observers(
        &self,
    ) -> impl Iterator<Item = crate::original_backing::MarkdownOriginalBackingObserver> + '_ {
        self.original_snapshot
            .iter()
            .map(|backing| backing.observer())
            .chain(
                self.edits.segments[..self.edits.published_segments]
                    .iter()
                    .filter_map(|segment| match segment.as_ref() {
                        // Heap segments have no mapping obligation; never invent
                        // an already-released observation for a nonexistent file resource.
                        MarkdownEditSegment::Heap(_) => None,
                        // Lend only this actual backing's observation without
                        // reading text or building another resource inventory.
                        MarkdownEditSegment::Frozen(backing) => Some(backing.observer()),
                        // Retirement ends only the document's strong payload edge;
                        // evidence for its published mapping remains under the same ID.
                        MarkdownEditSegment::RetiredFrozen(observer) => {
                            Some(observer.as_ref().clone())
                        }
                    }),
            )
    }

    /// Borrow only the current bounded leaf. An original payload's nonzero
    /// backing offset does not alter logical piece coordinates.
    fn piece_bytes(
        &self,
        piece: &MarkdownPiece,
    ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError> {
        self.piece_range_bytes(piece, 0..piece.byte_len)
    }

    /// Lease an exact local extent from the original backing or stable edit
    /// segment without changing the logical piece or its cached aggregate.
    fn piece_range_bytes(
        &self,
        piece: &MarkdownPiece,
        local: Range<usize>,
    ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError> {
        self.original_backing_health()?;
        if local.start > local.end || local.end > piece.byte_len {
            // Reject invalid local extents before acquiring any backing resource.
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        match piece.source {
            MarkdownPieceSource::Original => {
                // Original piece coordinates belong to this same immutable Markdown snapshot.
                // Keep health checks and lease only the actual local range, never the full backing.
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
                // Heap and frozen segments share the same stable piece coordinates;
                // a file-backed segment leases only this actual finite range.
                self.edits.read(
                    index,
                    piece.byte_start + local.start..piece.byte_start + local.end,
                )
            }
        }
    }
}

impl PieceByteSource for MarkdownDocumentBuffer {
    /// Forward health checks to the one active document's original and edited resources.
    fn source_health(&self) -> Result<(), MarkdownDocumentBufferError> {
        self.original_backing_health()
    }
    /// Keep whole-piece trait reads on the same finite active-source implementation.
    fn piece_bytes(
        &self,
        piece: &MarkdownPiece,
    ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError> {
        MarkdownDocumentBuffer::piece_bytes(self, piece)
    }

    /// Retain the real leaf and aggregate while lending only this quantum's
    /// requested local byte extent through the original finite lease.
    #[cfg(feature = "paged-shaping-frontier")]
    fn piece_range_bytes(
        &self,
        piece: &MarkdownPiece,
        local: Range<usize>,
    ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError> {
        MarkdownDocumentBuffer::piece_range_bytes(self, piece, local)
    }
}

/// Retain a shared root, selection, and unique real change ticket. The journal
/// page identity cannot be cloned into duplicate release authority.
///
/// Only tree roots, selection, and replacement metrics are retained; snapshots
/// never copy the document body, so bounded history does not duplicate full text.
struct MarkdownEditorMemento {
    root: PieceTreeLink,
    selection: MarkdownTextSelection,
    // Only composition groups record the actual counterpart selection. Ordinary
    // history preserves its established exchange with the current selection.
    counterpart_selection: Option<MarkdownTextSelection>,
    replacement: MarkdownReplacementMetrics,
    changes: change_journal::Entry,
}

impl MarkdownReplacementMetrics {
    /// Record a checked signed native length delta before accepting an editor replacement.
    fn new(
        location: usize,
        removed_utf16_length: usize,
        inserted_utf16_length: usize,
    ) -> Result<Self, MarkdownDocumentBufferError> {
        let forward_change_in_utf16_length =
            signed_length_change(inserted_utf16_length, removed_utf16_length)?;
        Ok(Self {
            location,
            removed_utf16_length,
            inserted_utf16_length,
            forward_change_in_utf16_length,
        })
    }

    /// Describe or perform an undo restoration using recorded replacement metrics and selection.
    fn undo_text_edit(self, selection_after: MarkdownTextSelection) -> MarkdownHistoryTextEdit {
        MarkdownHistoryTextEdit {
            replaced_utf16_range: self.location
                ..self.location.saturating_add(self.inserted_utf16_length),
            change_in_utf16_length: -self.forward_change_in_utf16_length,
            selection_after,
        }
    }

    /// Describe or perform a redo restoration using recorded replacement metrics and selection.
    fn redo_text_edit(self, selection_after: MarkdownTextSelection) -> MarkdownHistoryTextEdit {
        MarkdownHistoryTextEdit {
            replaced_utf16_range: self.location
                ..self.location.saturating_add(self.removed_utf16_length),
            change_in_utf16_length: self.forward_change_in_utf16_length,
            selection_after,
        }
    }
}

/// Convert an unsigned replacement size difference into the signed native notification delta.
fn signed_length_change(
    inserted_utf16_length: usize,
    removed_utf16_length: usize,
) -> Result<isize, MarkdownDocumentBufferError> {
    if inserted_utf16_length >= removed_utf16_length {
        // Growth and equal-size replacements use a nonnegative delta checked against isize.
        isize::try_from(inserted_utf16_length - removed_utf16_length).map_err(|_| {
            // The requested extent cannot fit the editor's supported byte or signed-coordinate limit.
            MarkdownDocumentBufferError::DocumentTooLarge
        })
    } else {
        // Shrinkage converts the magnitude first so negation cannot wrap an oversized unsigned value.
        isize::try_from(removed_utf16_length - inserted_utf16_length)
            .map(|difference| {
                // Convert an already checked shrinkage magnitude into the signed native length delta.
                -difference
            })
            .map_err(|_| {
                // The requested extent cannot fit the editor's supported byte or signed-coordinate limit.
                MarkdownDocumentBufferError::DocumentTooLarge
            })
    }
}

/// Retain recent shared roots while enforcing the fixed undo or redo entry limit.
fn push_bounded_snapshot(
    history: &mut VecDeque<MarkdownEditorMemento>,
    snapshot: MarkdownEditorMemento,
) -> Option<MarkdownEditorMemento> {
    // Detach the old ticket first; the caller releases its journal pages only
    // after the new bounded-history entry has committed successfully.
    let evicted = if history.len() == MAX_EDIT_HISTORY_ENTRIES {
        // Evict exactly the oldest entry when the real retained-history bound is reached.
        history.pop_front()
    } else {
        // Existing capacity does not create a fictitious journal-release obligation.
        None
    };
    history.push_back(snapshot);
    evicted
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Open-file identity baseline for detecting path replacement before persistence.
struct MarkdownFileIdentity {
    device: u64,
    inode: u64,
    byte_count: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
}

impl From<&Metadata> for MarkdownFileIdentity {
    /// Capture the file identity fields used to detect replacement and concurrent metadata changes.
    fn from(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            byte_count: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
        }
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
/// Stable rejection categories for safe storage, Unicode boundaries, and bounded editing.
pub enum MarkdownDocumentBufferError {
    /// Actual anonymous display-coordinate storage failure; None denotes revoked
    /// index authority or synchronization damage without inventing a system errno.
    #[error("显示坐标存储不可用（系统错误 {0:?}）")]
    DisplayStorageUnavailable(Option<i32>),
    /// Failed creation, writing, sealing, or reading of this unpublished anonymous
    /// candidate does not revoke an otherwise unchanged original Source.
    #[error("编辑候选存储不可用（系统错误 {0}）")]
    EditStorageUnavailable(i32),
    /// Damaged revision-cache synchronization explicitly rejects access; an old
    /// digest cannot claim that commit authority remains valid.
    #[error("正文版本摘要暂不可用")]
    RevisionCacheUnavailable,
    /// Unavailable derived-cache synchronization cannot be repaired by clearing
    /// poison and returning an uncertified old window.
    #[error("正文读取窗口暂不可用")]
    Utf16ReadCacheUnavailable,
    /// Actual original-backing acquisition, synchronous reclamation, or release
    /// failure carries only its fixed errno, never user content.
    #[error("原始正文映射不可用（系统错误 {0}）")]
    OriginalBackingUnavailable(i32),
    /// Bound simultaneously live original leases so recursive/concurrent reads
    /// cannot exceed their retained-source resource limit.
    #[error("同时读取的原始正文窗口过多")]
    OriginalReadLeaseLimitExceeded,

    /// Await completion of this same native input. Other writes cannot disturb
    /// the candidate append extent or its original selection authority.
    #[error("当前文本输入尚未完成，请稍后重试")]
    TextInputPrepared,
    /// A ticket no longer belongs to the unique current candidate, was cancelled,
    /// or lost its original canonical authority.
    #[error("当前文本输入的提交权限已失效")]
    PreparedTextInputStale,
    /// Reserving the sole history entry failed before native input began.
    #[error("无法为当前文本输入准备编辑历史")]
    EditHistoryUnavailable,
    #[error("当前文档的变化日志达到会话上限")]
    ChangeJournalLimitExceeded,
    #[error("无法完整准备或读取当前文档的变化日志")]
    ChangeJournalUnavailable,
    #[error("正文未提交，但变化日志失败后的物理清理未确认")]
    ChangeJournalCleanupUnconfirmed,
    #[error("Markdown 文件无法安全打开")]
    OpenUnavailable,
    #[error("Markdown 路径不是普通文件")]
    NotRegularFile,
    #[error("Markdown 文件超过编辑大小上限")]
    DocumentTooLarge,
    #[error("Markdown 文件不是有效 UTF-8")]
    InvalidUtf8Document,
    #[error("Markdown 文件在打开期间发生变化")]
    DocumentChangedDuringOpen,
    #[error("Markdown 文件在保存提交期间发生变化")]
    DocumentChangedDuringSave,
    #[error("文本范围越过文档边界")]
    RangeOutsideDocument,
    #[error("文本位置不在 Unicode 标量边界")]
    NotUnicodeBoundary,
    #[error("视口文本超过有界物化上限")]
    #[cfg(any(test, feature = "native-markdown-candidate"))]
    MaterializationLimitExceeded,
    #[error("当前编辑会话的追加缓冲已达到上限")]
    EditBufferLimitExceeded,
    #[error("Markdown 流式写入失败")]
    WriteUnavailable,
    /// Anonymous replacement input cannot continue reading; expose neither its
    /// path, content, nor internal system-error details.
    #[error("Markdown 替换输入无法完整读取")]
    ReplacementInputUnavailable,
    /// Original coordinates must be ascending and nonoverlapping. Consecutive
    /// empty insertions at one position cannot introduce ambiguous ordering.
    #[error("Markdown 替换范围未按原文顺序排列或发生重叠")]
    ReplacementRangesNotOrdered,
    /// The bounded find owner cancelled or revoked this request's deadline;
    /// partial enumeration cannot be reported as a complete result.
    #[error("Markdown 查找已中止")]
    FindInterrupted,
}

impl From<crate::original_backing::OriginalBackingError> for MarkdownDocumentBufferError {
    /// Preserve the concrete backing failure category at the document boundary
    /// without inferring a missing source or successful empty read from I/O failure.
    fn from(error: crate::original_backing::OriginalBackingError) -> Self {
        match error {
            crate::original_backing::OriginalBackingError::RangeOutsideMapping => {
                // An invalid backing extent remains a rejected document range.
                Self::RangeOutsideDocument
            }
            crate::original_backing::OriginalBackingError::Unavailable(errno) => {
                // Keep the original system cause rather than synthesizing another errno.
                Self::OriginalBackingUnavailable(errno)
            }
            crate::original_backing::OriginalBackingError::LeaseLimitExceeded => {
                // Resource contention is distinct from malformed coordinates or I/O failure.
                Self::OriginalReadLeaseLimitExceeded
            }
        }
    }
}

#[cfg(test)]
mod tests;
