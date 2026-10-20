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
// 原生运行启用有限表格权限；默认视口的迁移仍须分别完成语义、几何和功能门禁。
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
// 规范根只存通用Unicode属性事实；方向/布局快路的适用策略由各算法自行决定。
#[path = "markdown_document_buffer/unicode_properties.rs"]
mod canonical_unicode_properties;
pub(crate) use canonical_unicode_properties::MarkdownUnicodeProperties;
// 冻结文本只分享不可变正文资源；不携带活动文档、历史或保存权限。
#[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
pub(crate) mod display_projection;
mod frozen_text;
pub use frozen_text::MarkdownTextSnapshot;
#[cfg(feature = "paged-shaping-frontier")]
pub(crate) use frozen_text::{
    FrozenBidiParagraphCursor, FrozenPrintableAsciiSuffix, FrozenPrintableAsciiSuffixStep,
};
mod queries;
// 单个派生UTF-16窗口只加速有限读取；它不取得正文或历史所有权。
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
    // 独立匿名inode可由已发布不可变值共享；活动文档释放不会撤销旧值的读取。
    original_snapshot: Option<Arc<OriginalBacking>>,
    original_range: std::ops::Range<usize>,
    save_authority: MarkdownSaveAuthority,
    maximum_document_bytes: usize,
    root: PieceTreeLink,
    initial_root: PieceTreeLink,
    edits: MarkdownEditSegments,
    // 真实变化只保有限inline记录或文档自有匿名页；不与UTF8正文段混用。
    change_journal: change_journal::ChangeJournal,
    selection: MarkdownTextSelection,
    // 输入候选仍由同一文档独占；未发布期间所有写入口均拒绝，读取只见canonical。
    prepared_text_input: Option<prepared_text_input::PendingTextInput>,
    // 只描述当前Body组合的历史归属；正文仍只由原root、编辑段和Undo尾项拥有。
    text_input_composition: Option<prepared_text_input::CompositionGroup>,
    // 同一文档最多一个cell原始草稿；retired槽也须等真实读取根归还后才能再写canonical。
    #[cfg(feature = "s2-table-window-prototype")]
    cell_draft: Option<cell_draft::CellDraftSlot>,
    undo: VecDeque<MarkdownEditorMemento>,
    redo: VecDeque<MarkdownEditorMemento>,
    generation: u64,
    // 活动文档与全部独立capture共用同一8KiB派生窗口；不按快照数量重复分配。
    // 窗口只保原root的Weak键，不能经这个共享Arc反向保活文档、正文或历史。
    utf16_read_cache: Arc<Mutex<read_cache::Utf16ReadCache>>,
    // 只缓存同一不可变root的32B摘要；身份Weak不保活旧正文或历史。
    revision_cache: Mutex<revision_cache::RevisionCache>,
    #[cfg(any(test, feature = "native-markdown-candidate"))]
    block_checkpoints: RefCell<MarkdownBlockCheckpointIndex>,
}

#[derive(Clone, Copy)]
/// 磁盘基线与未知旧基线的恢复草稿不能互相冒充；恢复内容相同也不自动取得覆盖权限。
enum MarkdownSaveAuthority {
    /// 正常文件打开或完成保存后，identity与revision共同约束后续普通覆盖。
    DiskBaseline {
        identity: MarkdownFileIdentity,
        revision: MarkdownDocumentRevision,
    },
    /// 旧record只证明恢复内容身份，没有发生冲突前的disk baseline。
    PendingLegacyRecovery { record_digest: [u8; 32] },
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
            original_range: 0..byte_count,
            save_authority: MarkdownSaveAuthority::DiskBaseline {
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

    /// 采用已经固定的旧record正文区间；不复制正文，不把未知旧基线变为当前磁盘版本。
    pub(crate) fn from_legacy_recovery(
        path: &Path,
        snapshot: OriginalBacking,
        payload_range: std::ops::Range<usize>,
        record_digest: [u8; 32],
        maximum_document_bytes: u64,
    ) -> Result<Self, MarkdownDocumentBufferError> {
        let maximum_document_bytes = usize::try_from(maximum_document_bytes)
            .map_err(|_| MarkdownDocumentBufferError::DocumentTooLarge)?;
        if payload_range.start > payload_range.end || payload_range.end > snapshot.len() {
            // record范围必须属于同一已冻结inode；不借全片来检查payload。
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        if payload_range.len() > maximum_document_bytes {
            return Err(MarkdownDocumentBufferError::DocumentTooLarge);
        }
        let build = build_original_backing_tree(Some(&snapshot), payload_range.clone())?;
        let root = build.root;
        #[cfg(any(test, feature = "native-markdown-candidate"))]
        let block_checkpoints = build.checkpoints;
        Ok(Self {
            path: path.to_path_buf(),
            original_snapshot: Some(Arc::new(snapshot)),
            original_range: payload_range,
            save_authority: MarkdownSaveAuthority::PendingLegacyRecovery { record_digest },
            maximum_document_bytes,
            initial_root: root.clone(),
            revision_cache: Mutex::new(revision_cache::RevisionCache::seeded(
                &root,
                build.revision,
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
    /// 只返回真实已知的保存基线；旧恢复record没有base revision，None不能被当前磁盘hash替代。
    pub fn original_revision(&self) -> Option<MarkdownDocumentRevision> {
        match self.save_authority {
            // 普通打开或成功保存的authority才可作为后续revision检查基线。
            MarkdownSaveAuthority::DiskBaseline { revision, .. } => Some(revision),
            // 即使恢复正文与磁盘相同或为空，也必须先经过显式解决。
            MarkdownSaveAuthority::PendingLegacyRecovery { .. } => None,
        }
    }

    #[must_use]
    /// 只读显示待解决的旧恢复冲突；不能通过该状态推断某个磁盘版本已获覆盖授权。
    pub fn has_pending_legacy_recovery(&self) -> bool {
        matches!(
            self.save_authority,
            MarkdownSaveAuthority::PendingLegacyRecovery { .. }
        )
    }

    /// 内容身份只用于绑定小resolution marker，不包含正文或可变文件句柄。
    pub(crate) fn legacy_recovery_record_digest(&self) -> Option<[u8; 32]> {
        match self.save_authority {
            // 正常模型不产生marker，避免把一次普通打开写成隐式恢复决定。
            MarkdownSaveAuthority::DiskBaseline { .. } => None,
            // 摘要来自不可变record的完整bytes，文件换inode但内容相同仍是同一恢复身份。
            MarkdownSaveAuthority::PendingLegacyRecovery { record_digest } => Some(record_digest),
        }
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
            Ok(cache) => cache,
            Err(_) => {
                // 同步错误不能恢复旧缓存；此时也优先保留共享原映射的真实首个失败。
                self.original_backing_health()?;
                return Err(MarkdownDocumentBufferError::RevisionCacheUnavailable);
            }
        };
        cache.read(self, &self.root)
    }

    #[must_use]
    /// Conservatively detect path replacement or metadata changes before attempting a save.
    pub fn original_file_identity_is_unchanged(&self) -> bool {
        let MarkdownSaveAuthority::DiskBaseline { identity, .. } = self.save_authority else {
            // 未知旧基线不能因当前路径仍存在就被升级为可写authority。
            return false;
        };
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
        let MarkdownSaveAuthority::DiskBaseline { identity, .. } = self.save_authority else {
            // 一个发布后的正常save mapping才有重新绑定权限，恢复模型不能通过该入口解除保护。
            return Err(MarkdownDocumentBufferError::PendingLegacyRecovery);
        };
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
        if self.has_pending_legacy_recovery() {
            // 普通保存与相同bytes收敛都不能替用户解决未知基线的旧冲突。
            return Err(MarkdownDocumentBufferError::PendingLegacyRecovery);
        }
        let Some(saved_revision) = saved.original_revision() else {
            // 另一恢复草稿也不是新的磁盘保存基线。
            return Err(MarkdownDocumentBufferError::PendingLegacyRecovery);
        };
        if self.path != saved.path
            || !saved.original_file_identity_is_unchanged()
            || self.current_revision()? != saved_revision
        {
            // A raced destination or different content cannot authorize changing this draft's save baseline.
            return Err(MarkdownDocumentBufferError::DocumentChangedDuringSave);
        }
        // Only validated metadata and one shared root become the new conflict/dirty baseline.
        self.save_authority = saved.save_authority;
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
    /// 冻结当前canonical正文；仅复制资源引用与索引，不复制正文或保活活动文档。
    ///
    /// 已准备但未发布的输入不在当前root中。后续编辑、Undo/Redo、路径变化及文档释放
    /// 都不会改变此值；快照的最后一个所有者释放后才归还其仍引用的匿名映射与编辑段。
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
        self.text_input_composition = None; // 明确程序选区是组合断点，不把后来同位置的新输入吞进旧组。
        Ok(())
    }

    #[must_use]
    /// 普通模型按共享root识别保存状态；旧恢复即使空值或Undo回初值也始终dirty，不在输入路径hash正文。
    pub fn is_dirty(&self) -> bool {
        self.has_pending_legacy_recovery()
            || !piece_tree_links_share_root(&self.root, &self.initial_root)
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

    /// 既有入口也记录真实变化；不需要派生结果的caller使用纯Noop观察者。
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

    /// 在提交前流式观察真实旧坐标变化；观察者失败保留原root、选区、append预算及两侧历史。
    pub fn replace_utf16_range_with_selection_observed<O: changes::MarkdownTextChangeObserver>(
        &mut self,
        range: Range<usize>,
        replacement: &str,
        selection_before: MarkdownTextSelection,
        selection_after: MarkdownTextSelection,
        observer: O,
    ) -> Result<O::Output, changes::MarkdownObservedEditError<O::Error>> {
        // 单阶段兼容入口复用同一准备/发布切口；失败仍发生在canonical切换之前。
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

    /// 兼容入口在日志损坏/I/O失败时不移动history；需要typed失败的caller使用observed入口。
    pub fn undo_text_edit(&mut self) -> Option<MarkdownHistoryTextEdit> {
        self.undo_text_edit_observed(changes::NoopObserver)
            .ok()
            .flatten()
            .map(|(edit, ())| edit)
    }

    /// 先peek逆变化并完成派生状态，再原子移动原root和同一日志票据；失败不pop历史。
    pub fn undo_text_edit_observed<O: changes::MarkdownTextChangeObserver>(
        &mut self,
        observer: O,
    ) -> changes::MarkdownObservedHistoryResult<O> {
        self.restore_history_observed(true, observer)
    }

    #[cfg(any(test, feature = "native-markdown-candidate"))]
    /// 便利入口只取恢复后的选区；读取日志失败仍保持原状态。
    pub fn redo(&mut self) -> Option<MarkdownTextSelection> {
        self.redo_text_edit().map(|edit| edit.selection_after)
    }

    /// 兼容入口不把读取失败当已完成Redo；原history和正文保持可重试。
    pub fn redo_text_edit(&mut self) -> Option<MarkdownHistoryTextEdit> {
        self.redo_text_edit_observed(changes::NoopObserver)
            .ok()
            .flatten()
            .map(|(edit, ())| edit)
    }

    /// 正向变化流重放完成前不pop；每条变化仍使用该历史事务的旧坐标。
    pub fn redo_text_edit_observed<O: changes::MarkdownTextChangeObserver>(
        &mut self,
        observer: O,
    ) -> changes::MarkdownObservedHistoryResult<O> {
        self.restore_history_observed(false, observer)
    }

    fn restore_history_observed<O: changes::MarkdownTextChangeObserver>(
        &mut self,
        inverse: bool,
        mut observer: O,
    ) -> changes::MarkdownObservedHistoryResult<O> {
        self.ensure_text_input_idle()?;
        let source = if inverse { &self.undo } else { &self.redo };
        let Some(target) = source.back() else {
            return Ok(None); // 无历史不调用观察者，也不产生虚构变化。
        };
        self.change_journal
            .visit(&target.changes, inverse, |record| {
                observer.changed(changes::MarkdownTextChange {
                    old_utf16_range: record.start..record.start + record.removed,
                    inserted_utf16_len: record.inserted,
                })
            })
            .map_err(|error| match error {
                // 任意读/校验失败都在peek阶段停止；观察者的部分候选随错误丢弃。
                change_journal::VisitError::Journal(error) => {
                    changes::MarkdownObservedEditError::Journal(error)
                }
                change_journal::VisitError::Observer(error) => {
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
        // 所有可失败步骤结束后才从源history取走唯一票据，向另一侧转移而不复制日志页。
        let target = if inverse {
            self.undo.pop_back()
        } else {
            self.redo.pop_back()
        }
        .expect("the immutable peek validated one existing history entry");
        let opposite = MarkdownEditorMemento {
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
            self.change_journal.release(evicted.changes);
        }
        self.root = target.root;
        self.selection = target.selection;
        self.text_input_composition = None; // 真正历史恢复结束旧组合分组，失败的peek阶段不会改此状态。
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

    /// 读取NSString兼容单元；单个派生窗口最多4096单元，不建立整篇编码镜像。
    pub fn utf16_code_unit_at(
        &self,
        utf16_offset: usize,
    ) -> Result<u16, MarkdownDocumentBufferError> {
        // 健康失败优先于缓存命中；EOF没有单元，且不能因缓存中旧字节而成功。
        self.original_backing_health()?;
        if utf16_offset >= self.utf16_len() {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        let mut unit = [0];
        self.copy_utf16_code_units(utf16_offset, &mut unit)?;
        Ok(unit[0])
    }

    /// 完整填充调用方范围，允许从低代理项开始；内部一次只保留一个8KiB窗口。
    ///
    /// 大请求仍按原真实范围逐窗完成，不截断、不改变NSString语义；原root与generation
    /// 每次读取都参与命中验证，编辑、Undo或失败事务临时换root不能复用旧窗口。
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
        // 借用只覆盖纯Rust读取；没有Native调用或活动文档回链藏在cache中。
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
            // 原扫描器只消费完整 CR/LF 物理行；唯一行被任一预算截断且未到 EOF 时，
            // 首轮就会在 consume_line 之前停止，既没有完整 block，也不产生新 checkpoint。
            // 直接走原 source fallback，省掉注定会被丢弃的最大扫描 String；不改变语法判定。
            // desired_end=0 的原空请求仍保留下面的 Complete 空值路径，不能改签为 fallback。
            self.byte_offset_for_utf16(scan_end)?; // UTF-16 上限可能切在 surrogate 中间；保留旧扫描切片的边界错误。
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

    /// 将真实UTF-16选区按原UTF-8字节流式写出，不创建正文String或选区镜像。
    ///
    /// 两端必须是标量边界；全部范围预检完成前不调用writer。空范围和EOF同样核原源健康。
    /// writer失败可能已经留下部分输出，但不改变canonical、选择或历史；原叶lease的真实
    /// 回收错误优先于写入错误，调用方不得把不完整目标当作已经成功复制。
    pub fn write_utf16_range_to(
        &self,
        range: Range<usize>,
        destination: &mut impl Write,
    ) -> Result<(), MarkdownDocumentBufferError> {
        self.original_backing_health()?;
        if range.start > range.end || range.end > self.utf16_len() {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        } // 拒绝逆序和越界，不能夹取范围后写入另一段正文。
        let start = self.byte_offset_for_utf16(range.start)?;
        let end = self.byte_offset_for_utf16(range.end)?;
        // 两次转换先排除代理对内部端点；随后只借选区相交的有限叶，UTF8字节保持原值。
        visit_piece_tree_range(self, self.root.as_deref(), start..end, 0, &mut |bytes| {
            destination
                .write_all(bytes)
                .map_err(|_| MarkdownDocumentBufferError::WriteUnavailable)
        }) // 原visitor先归还真实lease再传播writer结果，不能用写入失败洗掉Source首错。
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

    #[cfg(test)]
    pub(crate) fn fail_next_original_reclaim_for_test(&self) {
        if let Some(backing) = &self.original_snapshot {
            backing.fail_next_reclaim_for_test();
        }
    }

    #[cfg(test)]
    /// 测试显式触发真实无pin页驱逐；不能把注入标志本身当成已发生I/O错误。
    pub(crate) fn reclaim_original_read_windows_for_test(
        &self,
    ) -> Result<(), MarkdownDocumentBufferError> {
        match self.original_snapshot.as_ref() {
            Some(backing) => backing.reclaim_unpinned_for_test().map_err(Into::into),
            None => Ok(()), // 纯编辑或空原文没有原始映射页需要驱逐。
        }
    }

    /// 原回收失败对所有后继读取和提交永久生效；metadata长度本身不授予新的正文权限。
    pub(crate) fn original_backing_health(&self) -> Result<(), MarkdownDocumentBufferError> {
        if let Some(backing) = self.original_snapshot.as_deref() {
            // 原始Source首错仍先于后继编辑文件；两者都不能被纯metadata查询绕过。
            backing.health()?;
        }
        self.edits.health()
    }

    /// 观察原始映射的真实最终VM释放；固定标量observer不保活正文资源。
    #[must_use]
    pub fn original_backing_observer(
        &self,
    ) -> Option<crate::original_backing::MarkdownOriginalBackingObserver> {
        self.original_snapshot
            .as_ref()
            .map(|backing| backing.observer())
    }

    /// 遍历文档实际持有的原始映射及已发布文件段；观察票不保活正文、文件或映射。
    ///
    /// 已发布段即使暂时只由Undo/Redo引用，也仍归该文档释放；未发布候选仍归原事务。
    /// 调用者须在销毁文档前取得这些票，再于真实最后owner退场后检查VM释放结果。
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
                        // 堆段不承担文件映射义务，不能合成一个已释放票冒充实际资源。
                        MarkdownEditSegment::Heap(_) => None,
                        // 同一实际backing只借出票；不读取正文、不建立第二份存储库存。
                        MarkdownEditSegment::Frozen(backing) => Some(backing.observer()),
                        // 退休只结束Doc的payload强边；全部已发布映射的释放证据仍在同一个编号。
                        MarkdownEditSegment::RetiredFrozen(observer) => {
                            Some(observer.as_ref().clone())
                        }
                    }),
            )
    }

    /// 每次借用只取得当前叶的有限权限；原始payload偏移不改变piece的逻辑坐标。
    fn piece_bytes(
        &self,
        piece: &MarkdownPiece,
    ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError> {
        self.piece_range_bytes(piece, 0..piece.byte_len)
    }

    fn piece_range_bytes(
        &self,
        piece: &MarkdownPiece,
        local: Range<usize>,
    ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError> {
        self.original_backing_health()?;
        if local.start > local.end || local.end > piece.byte_len {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        match piece.source {
            MarkdownPieceSource::Original => {
                let backing = self
                    .original_snapshot
                    .as_ref()
                    .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
                let start = self
                    .original_range
                    .start
                    .checked_add(piece.byte_start)
                    .and_then(|start| start.checked_add(local.start))
                    .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
                backing
                    .lease(start..start + local.len())
                    .map(PieceBytes::Original)
                    .map_err(Into::into)
            }
            MarkdownPieceSource::EditSegment(index) => {
                // 同一编号的堆段和封存段共用piece坐标；映射分支只取得当前有限range的lease。
                self.edits.read(
                    index,
                    piece.byte_start + local.start..piece.byte_start + local.end,
                )
            }
        }
    }
}

impl PieceByteSource for MarkdownDocumentBuffer {
    fn source_health(&self) -> Result<(), MarkdownDocumentBufferError> {
        self.original_backing_health()
    }
    fn piece_bytes(
        &self,
        piece: &MarkdownPiece,
    ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError> {
        MarkdownDocumentBuffer::piece_bytes(self, piece)
    }

    /// 保留原叶与其真实摘要，只把本量子的局部字节范围交给原有限lease。
    #[cfg(feature = "paged-shaping-frontier")]
    fn piece_range_bytes(
        &self,
        piece: &MarkdownPiece,
        local: Range<usize>,
    ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError> {
        MarkdownDocumentBuffer::piece_range_bytes(self, piece, local)
    }
}

/// 共享root、选区与唯一真实变化票据；页身份不可Clone，避免重复释放。
///
/// Only tree roots, selection, and replacement metrics are retained; snapshots
/// never copy the document body, so bounded history does not duplicate full text.
struct MarkdownEditorMemento {
    root: PieceTreeLink,
    selection: MarkdownTextSelection,
    // 仅组合组记录真实另一端选区；普通历史继续沿原当前选择交换合同。
    counterpart_selection: Option<MarkdownTextSelection>,
    replacement: MarkdownReplacementMetrics,
    changes: change_journal::Entry,
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
) -> Option<MarkdownEditorMemento> {
    // 先从容器取出旧票据，调用方仅在新提交成功后释放其日志页。
    let evicted = if history.len() == MAX_EDIT_HISTORY_ENTRIES {
        history.pop_front()
    } else {
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
    /// 只读显示来源的匿名坐标目录真实失败；None 表示索引撤权或同步状态损坏，不伪造 errno。
    #[error("显示坐标存储不可用（系统错误 {0:?}）")]
    DisplayStorageUnavailable(Option<i32>),
    /// 本次匿名候选尚未发布时的真实创建、写入、封存或读取失败；不撤销未改原Source。
    #[error("编辑候选存储不可用（系统错误 {0}）")]
    EditStorageUnavailable(i32),
    /// 摘要缓存的同步状态损坏时明确拒绝，不以旧摘要宣称提交权限仍有效。
    #[error("正文版本摘要暂不可用")]
    RevisionCacheUnavailable,
    /// 派生读取窗口的同步状态不可用；不能清除poison后继续返回未经认证的缓存。
    #[error("正文读取窗口暂不可用")]
    Utf16ReadCacheUnavailable,
    /// 原始匿名映射的真实获取、同步归还或释放失败；errno固定且不携带用户数据。
    #[error("原始正文映射不可用（系统错误 {0}）")]
    OriginalBackingUnavailable(i32),
    /// 限制同时存活的原始读取窗口，避免递归或并发借用突破驻留上界。
    #[error("同时读取的原始正文窗口过多")]
    OriginalReadLeaseLimitExceeded,

    /// 正在等待同一原生输入完成；不能让其他写操作破坏候选append与原始选区权限。
    #[error("当前文本输入尚未完成，请稍后重试")]
    TextInputPrepared,
    /// 票据不属于当前唯一候选、已取消或原始canonical权限已撤销。
    #[error("当前文本输入的提交权限已失效")]
    PreparedTextInputStale,
    /// 进入原生输入前无法为唯一历史记录预留容量。
    #[error("无法为当前文本输入准备编辑历史")]
    EditHistoryUnavailable,
    #[error("当前文档的变化日志达到会话上限")]
    ChangeJournalLimitExceeded,
    #[error("无法完整准备或读取当前文档的变化日志")]
    ChangeJournalUnavailable,
    #[error("正文未提交，但变化日志失败后的物理清理未确认")]
    ChangeJournalCleanupUnconfirmed,
    #[error("旧冲突草稿尚未解决，请另存为新笔记或明确重新载入磁盘版本")]
    PendingLegacyRecovery,
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
    /// 匿名替换输入无法继续读取；不携带路径、正文或系统错误详情。
    #[error("Markdown 替换输入无法完整读取")]
    ReplacementInputUnavailable,
    /// 原文坐标必须升序且不重叠，同位置的连续空插入也不能制造不明确的次序。
    #[error("Markdown 替换范围未按原文顺序排列或发生重叠")]
    ReplacementRangesNotOrdered,
    /// 有界查找的调用方已取消或撤销本次期限，不把部分枚举冒充完整结果。
    #[error("Markdown 查找已中止")]
    FindInterrupted,
}

impl From<crate::original_backing::OriginalBackingError> for MarkdownDocumentBufferError {
    fn from(error: crate::original_backing::OriginalBackingError) -> Self {
        match error {
            crate::original_backing::OriginalBackingError::RangeOutsideMapping => {
                Self::RangeOutsideDocument
            }
            crate::original_backing::OriginalBackingError::Unavailable(errno) => {
                Self::OriginalBackingUnavailable(errno)
            }
            crate::original_backing::OriginalBackingError::LeaseLimitExceeded => {
                Self::OriginalReadLeaseLimitExceeded
            }
        }
    }
}

#[cfg(test)]
mod tests;
