//! Active mmap/piece-tree document API, edit history, and bounded viewport access.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs::{Metadata, OpenOptions};
use std::io::Write;
use std::ops::Range;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use memmap2::{Advice, Mmap, UncheckedAdvice};

use crate::immutable_snapshot::{SnapshotError, snapshot_file};
use sha2::{Digest, Sha256};
use thiserror::Error;

mod block_scan;
mod command_transaction;
mod editing_commands;
mod formatting;

pub use editing_commands::{MarkdownEditingCommand, MarkdownEditingOutcome};

pub use formatting::MarkdownFormatAction;
mod piece_tree;
mod queries;

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

/// A selection is stored in the same UTF-16 coordinate space used by TextKit.
/// Both positions must be Unicode scalar boundaries before an edit is accepted.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MarkdownTextSelection {
    pub anchor_utf16: usize,
    pub head_utf16: usize,
}

impl MarkdownTextSelection {
    #[must_use]
    /// Construct a collapsed TextKit selection without accessing document storage.
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
/// Compact replacement geometry sufficient to notify TextKit during history restoration.
struct MarkdownReplacementMetrics {
    location: usize,
    removed_utf16_length: usize,
    inserted_utf16_length: usize,
    forward_change_in_utf16_length: isize,
}

/// Exact character-edit notification required when TextKit restores a Rust history entry.
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
    original_snapshot: Option<Mmap>,
    original_identity: MarkdownFileIdentity,
    original_revision: MarkdownDocumentRevision,
    maximum_document_bytes: usize,
    root: PieceTreeLink,
    initial_root: PieceTreeLink,
    edits: MarkdownEditSegments,
    selection: MarkdownTextSelection,
    undo: VecDeque<MarkdownEditorMemento>,
    redo: VecDeque<MarkdownEditorMemento>,
    generation: u64,
    #[cfg(any(test, feature = "native-markdown-candidate"))]
    block_checkpoints: RefCell<MarkdownBlockCheckpointIndex>,
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
        let original_snapshot = snapshot_file(&file, before.len(), || {
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
        let original_bytes = original_snapshot.as_deref().unwrap_or_default();
        let original_text = std::str::from_utf8(original_bytes).map_err(|_| {
            // Invalid source encoding must remain an explicit failure rather than silently replacing bytes.
            MarkdownDocumentBufferError::InvalidUtf8Document
        })?;
        let (root, original_revision) = build_original_piece_tree(original_text);
        #[cfg(any(test, feature = "native-markdown-candidate"))]
        let block_checkpoints = MarkdownBlockCheckpointIndex::from_text(original_text);

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

        if let Some(mapping) = &original_snapshot {
            // Release clean indexed pages opportunistically after validation; advice failure does not affect correctness.
            let _ = mapping.advise(Advice::Random);
            // SAFETY: no slice is live here. The unlinked snapshot inode is immutable and exclusively
            // owned by this mapping, so clean pages may be faulted back without source-file mutation.
            // This advice is a best-effort residency hint and never changes the mapped bytes.
            let _ = unsafe { mapping.unchecked_advise(UncheckedAdvice::DontNeed) };
        }

        // Transfer all mapping and history ownership into the active document only after validation.
        Ok(Self {
            path: path.to_path_buf(),
            original_snapshot,
            original_identity,
            original_revision,
            maximum_document_bytes,
            initial_root: root.clone(),
            root,
            edits: MarkdownEditSegments::default(),
            selection: MarkdownTextSelection::default(),
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            generation: 0,
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
    /// Read the revision recorded at open or the latest successful save without changing edit history.
    pub fn original_revision(&self) -> MarkdownDocumentRevision {
        self.original_revision
    }

    /// Computes the current edited revision without materializing a flat
    /// document. This full traversal is reserved for explicit persistence and
    /// conflict recovery boundaries; it is never part of the input hot path.
    #[must_use]
    /// Hash live piece bytes only at an explicit persistence boundary.
    pub fn current_revision(&self) -> MarkdownDocumentRevision {
        let mut hasher = Sha256::new();
        hash_piece_tree(self, self.root.as_deref(), &mut hasher);
        MarkdownDocumentRevision(hasher.finalize().into())
    }

    #[must_use]
    /// Conservatively detect path replacement or metadata changes before attempting a save.
    pub fn original_file_identity_is_unchanged(&self) -> bool {
        let Ok(metadata) = std::fs::symlink_metadata(&self.path) else {
            // An unreadable or missing path cannot establish that the opened file is unchanged.
            return false;
        };
        !metadata.file_type().is_symlink()
            && metadata.is_file()
            && MarkdownFileIdentity::from(&metadata) == self.original_identity
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
        let metadata = std::fs::symlink_metadata(destination).map_err(|_| {
            // A failed destination identity inspection cannot establish ownership of the newly saved file.
            MarkdownDocumentBufferError::DocumentChangedDuringSave
        })?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || MarkdownFileIdentity::from(&metadata) != self.original_identity
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
        if self.path != saved.path
            || !saved.original_file_identity_is_unchanged()
            || self.current_revision() != saved.original_revision
        {
            // A raced destination or different content cannot authorize changing this draft's save baseline.
            return Err(MarkdownDocumentBufferError::DocumentChangedDuringSave);
        }
        // Only validated metadata and one shared root become the new conflict/dirty baseline.
        self.original_identity = saved.original_identity;
        self.original_revision = saved.original_revision;
        self.initial_root = self.root.clone();
        Ok(())
    }

    #[must_use]
    /// Read the exact live byte count from the root aggregate in constant time.
    pub fn byte_len(&self) -> usize {
        piece_tree_metrics(&self.root).byte_len
    }

    #[must_use]
    /// Read the TextKit coordinate length without materializing UTF-16 storage.
    pub fn utf16_len(&self) -> usize {
        piece_tree_metrics(&self.root).utf16_len
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
        self.validate_selection(selection)?;
        self.selection = selection;
        Ok(())
    }

    #[must_use]
    /// Recognize the saved persistent root without hashing the document on the input path.
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

    /// Apply a native text-system edit with an explicit pre-edit selection.
    ///
    /// Validate all inputs before changing text or selection. TextKit owns its
    /// current selection, and a rejected size/boundary check must not create a
    /// partially committed Rust edit or corrupt the undo snapshot.
    /// Validate text, size, and both selections before committing one native edit.
    pub fn replace_utf16_range_with_selection(
        &mut self,
        range: Range<usize>,
        replacement: &str,
        selection_before: MarkdownTextSelection,
        selection_after: MarkdownTextSelection,
    ) -> Result<(), MarkdownDocumentBufferError> {
        if range.start > range.end || range.end > self.utf16_len() {
            // Reject reversed or oversized edit ranges without modifying selection, tree, or history.
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        self.validate_selection(selection_before)?;
        // Validate current boundaries and future selection geometry before appending any edit storage.
        let byte_start = self.byte_offset_for_utf16(range.start)?;
        let byte_end = self.byte_offset_for_utf16(range.end)?;
        let replacement_utf16_len = replacement.encode_utf16().count();
        let replacement_metrics = MarkdownReplacementMetrics::new(
            range.start,
            range.end - range.start,
            replacement_utf16_len,
        )?;
        let result_utf16_len = self
            .utf16_len()
            .saturating_sub(range.end - range.start)
            .saturating_add(replacement_utf16_len);
        self.validate_selection_for_result(selection_after, result_utf16_len)?;
        self.validate_post_edit_utf16_boundary(
            selection_after.anchor_utf16,
            &range,
            replacement,
            replacement_utf16_len,
        )?;
        self.validate_post_edit_utf16_boundary(
            selection_after.head_utf16,
            &range,
            replacement,
            replacement_utf16_len,
        )?;
        self.replace_byte_range(
            byte_start..byte_end,
            replacement,
            selection_after,
            selection_before,
            replacement_metrics,
        )
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

    /// Describe or perform an undo restoration using recorded replacement metrics and selection.
    pub fn undo_text_edit(&mut self) -> Option<MarkdownHistoryTextEdit> {
        // Empty history has no effect; otherwise move the current shared root into the opposite history.
        let previous = self.undo.pop_back()?;
        push_bounded_snapshot(
            &mut self.redo,
            MarkdownEditorMemento {
                root: self.root.clone(),
                selection: self.selection,
                replacement: previous.replacement,
            },
        );
        // Restore the prior root and invalidate only scanner state following the changed location.
        self.root = previous.root;
        self.selection = previous.selection;
        self.generation = self.generation.wrapping_add(1);
        #[cfg(any(test, feature = "native-markdown-candidate"))]
        self.block_checkpoints
            .get_mut()
            .invalidate_after(previous.replacement.location);
        Some(previous.replacement.undo_text_edit(self.selection))
    }

    #[cfg(any(test, feature = "native-markdown-candidate"))]
    /// Reapply the next editor state and expose its selection to the native caller.
    pub fn redo(&mut self) -> Option<MarkdownTextSelection> {
        self.redo_text_edit().map(|edit| {
            // Expose only the restored selection through the convenience API; exact edit geometry remains available separately.
            edit.selection_after
        })
    }

    /// Describe or perform a redo restoration using recorded replacement metrics and selection.
    pub fn redo_text_edit(&mut self) -> Option<MarkdownHistoryTextEdit> {
        // Retain the current shared root before restoring the next history state; empty redo is a no-op.
        let next = self.redo.pop_back()?;
        push_bounded_snapshot(
            &mut self.undo,
            MarkdownEditorMemento {
                root: self.root.clone(),
                selection: self.selection,
                replacement: next.replacement,
            },
        );
        // Publish the restored selection and a new generation without copying document bytes.
        self.root = next.root;
        self.selection = next.selection;
        self.generation = self.generation.wrapping_add(1);
        #[cfg(any(test, feature = "native-markdown-candidate"))]
        self.block_checkpoints
            .get_mut()
            .invalidate_after(next.replacement.location);
        Some(next.replacement.redo_text_edit(self.selection))
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

    /// Read an NSString-compatible code unit without allocating a full UTF-16 copy.
    pub fn utf16_code_unit_at(
        &self,
        utf16_offset: usize,
    ) -> Result<u16, MarkdownDocumentBufferError> {
        if utf16_offset >= self.utf16_len() {
            // The end boundary is not a readable code unit and must be rejected.
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        utf16_code_unit_in_tree(self, self.root.as_deref(), utf16_offset)
    }

    /// Copies the requested UTF-16 code units directly from piece leaves.
    ///
    /// TextKit's `NSString` primitive may request a range that starts between
    /// the two code units of a surrogate pair, so this deliberately operates
    /// in UTF-16 coordinates rather than requiring Unicode scalar boundaries.
    /// The traversal skips unrelated AVL subtrees and does not materialize a
    /// temporary `String` or a second full-document buffer.
    /// Fill caller storage from a bounded UTF-16 range, including surrogate halves.
    pub fn copy_utf16_code_units(
        &self,
        utf16_offset: usize,
        destination: &mut [u16],
    ) -> Result<(), MarkdownDocumentBufferError> {
        // Checked arithmetic rejects overflow before lending any destination slice to tree traversal.
        let utf16_end = utf16_offset
            .checked_add(destination.len())
            .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
        if utf16_end > self.utf16_len() {
            // An overflow or end beyond the document cannot be serviced by the caller's requested slice.
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        let mut written = 0;
        copy_utf16_tree_range(
            self,
            self.root.as_deref(),
            utf16_offset..utf16_end,
            0,
            destination,
            &mut written,
        )?;
        if written != destination.len() {
            // A short traversal indicates an invalid range; never present a partial fill as successful.
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        Ok(())
    }

    #[cfg(any(test, feature = "native-markdown-candidate"))]
    /// Locate a logical line start using cached break counts and CRLF-aware descent.
    pub fn byte_offset_for_line(
        &self,
        zero_based_line: usize,
    ) -> Result<usize, MarkdownDocumentBufferError> {
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

    /// Commit one validated persistent-tree replacement and invalidate dependent editor state.
    fn replace_byte_range(
        &mut self,
        range: Range<usize>,
        replacement: &str,
        selection_after: MarkdownTextSelection,
        selection_before: MarkdownTextSelection,
        replacement_metrics: MarkdownReplacementMetrics,
    ) -> Result<(), MarkdownDocumentBufferError> {
        if range.start > range.end || range.end > self.byte_len() {
            // Reject an invalid byte interval before any edit segment is appended.
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        self.utf16_offset_for_byte(range.start)?;
        self.utf16_offset_for_byte(range.end)?;
        let result_bytes = self
            .byte_len()
            .saturating_sub(range.end - range.start)
            .checked_add(replacement.len())
            .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?;
        if result_bytes > self.maximum_document_bytes {
            // Enforce the resulting live-document byte cap independently of lifetime edit capacity.
            return Err(MarkdownDocumentBufferError::DocumentTooLarge);
        }
        if range.is_empty() && replacement.is_empty() {
            // A textually empty operation changes selection only and creates no undo entry or generation.
            self.selection = selection_after;
            return Ok(());
        }

        // Append immutable replacement bytes only after all range and capacity validation has succeeded.
        let inserted_pieces = self
            .edits
            .append(replacement, self.maximum_document_bytes)?;
        let inserted_root = build_balanced_piece_tree(inserted_pieces);
        let previous = MarkdownEditorMemento {
            root: self.root.clone(),
            selection: selection_before,
            replacement: replacement_metrics,
        };
        // Path-copy the two split boundaries, then publish one joined root and its bounded undo snapshot.
        let (prefix, remainder) = split_piece_tree(self, self.root.clone(), range.start)?;
        let (_, suffix) = split_piece_tree(self, remainder, range.end - range.start)?;
        self.root = join_piece_trees(prefix, join_piece_trees(inserted_root, suffix));
        self.selection = selection_after;
        push_bounded_snapshot(&mut self.undo, previous);
        // A new forward edit invalidates redo ownership and all dependent scanner state.
        self.redo.clear();
        self.generation = self.generation.wrapping_add(1);
        #[cfg(any(test, feature = "native-markdown-candidate"))]
        self.block_checkpoints
            .get_mut()
            .invalidate_after(replacement_metrics.location);
        Ok(())
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

    /// Borrow one leaf's immutable bytes from storage owned by this document.
    fn piece_bytes<'a>(&'a self, piece: &MarkdownPiece) -> &'a [u8] {
        let source = match piece.source {
            // Original leaves require the read-only mapping retained for the full document lifetime.
            MarkdownPieceSource::Original => self
                .original_snapshot
                .as_deref()
                .expect("an original piece requires a mapping"),
            // Edited leaves borrow an append-only segment that outlives every history root.
            MarkdownPieceSource::EditSegment(segment_index) => &self.edits.segments[segment_index],
        };
        &source[piece.byte_start..piece.byte_start + piece.byte_len]
    }
}

#[derive(Clone)]
/// Structurally shared editor memento with bounded history ownership.
///
/// Only tree roots, selection, and replacement metrics are retained; snapshots
/// never copy the document body, so bounded history does not duplicate full text.
struct MarkdownEditorMemento {
    root: PieceTreeLink,
    selection: MarkdownTextSelection,
    replacement: MarkdownReplacementMetrics,
}

impl MarkdownReplacementMetrics {
    /// Record a checked signed TextKit length delta before accepting an editor replacement.
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
) {
    if history.len() == MAX_EDIT_HISTORY_ENTRIES {
        // Evict the oldest snapshot before insertion when the history reaches its entry budget.
        history.pop_front();
    }
    history.push_back(snapshot);
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
}

#[cfg(test)]
mod tests;
