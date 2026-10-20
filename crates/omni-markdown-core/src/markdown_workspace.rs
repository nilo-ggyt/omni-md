//! Explicit vault sessions, confined file mutations, and safe persistence authority.

use std::cmp::Ordering;
use std::ffi::CString;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

mod attachments;
pub use attachments::MarkdownPendingAttachmentImport;
#[cfg(feature = "native-markdown-candidate")]
mod legacy_recovery;
#[cfg(feature = "native-markdown-candidate")]
mod native_save;
#[cfg(feature = "native-markdown-candidate")]
pub use legacy_recovery::{
    MarkdownRecoveryHandoff, MarkdownRecoveryPreparation, MarkdownRecoveryRecording,
    MarkdownRecoveryResolutionOutcome,
};
mod paging;
mod paths;
pub use paging::{
    MarkdownWorkspaceEntryKind, MarkdownWorkspacePage, MarkdownWorkspacePageAuthority,
    MarkdownWorkspacePageProgress, MarkdownWorkspacePager, compare_markdown_workspace_entries,
};
mod search;
pub use search::{MarkdownSearchAuthority, MarkdownSearchCursor, MarkdownSearchPage};
mod storage;
mod structure_publication;
use structure_publication::WorkspaceStructurePublication;

use attachments::*;
use paths::*;
use storage::*;

#[cfg(feature = "native-markdown-candidate")]
use super::markdown_document_buffer::{MarkdownDocumentBuffer, MarkdownDocumentBufferError};

pub const MARKDOWN_ROOT_DIRECTORY_NAME: &str = "OMNI";

const ATTACHMENTS_DIRECTORY_NAME: &str = "attachments";
const CONFLICT_RECOVERY_DIRECTORY_NAME: &str = "markdown-conflict-recovery";
const CONFLICT_DRAFT_MAGIC: &[u8; 8] = b"OMNIMD1\0";
const CONFLICT_DRAFT_HEADER_BYTES: usize = 20;
const MAX_RECOVERY_RELATIVE_PATH_BYTES: usize = 4_096;
const MAX_DOCUMENT_BYTES: u64 = 32 * 1_024 * 1_024;
const MAX_ATTACHMENT_BYTES: u64 = 256 * 1_024 * 1_024;
const MAX_SEARCH_FILE_BYTES: u64 = 128 * 1_024 * 1_024;
const MAX_QUERY_CHARACTERS: usize = 128;
const MAX_SEARCH_RESULTS: usize = 50;
const MAX_NOTE_TITLE_CHARACTERS: usize = 120;
const MAX_DIRECTORY_NAME_CHARACTERS: usize = 120;
const MAX_ATTACHMENT_NAME_CHARACTERS: usize = 200;
const SNIPPET_CONTEXT_CHARACTERS: usize = 72;
const SEARCH_CANCELLATION_BYTE_INTERVAL: usize = 16 * 1_024;
const ATTACHMENT_COPY_BUFFER_BYTES: usize = 64 * 1_024;
const TEMPORARY_SAVE_ATTEMPTS: usize = 32;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
/// Sidebar metadata without a retained note body or file handle.
pub struct MarkdownNoteSummary {
    pub relative_path: String,
    pub title: String,
    pub byte_count: u64,
    pub modified_at_ms: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
/// Owned listing result; traversal descriptors have been released before publication.
pub struct MarkdownWorkspaceSnapshot {
    pub directories: Vec<String>,
    pub notes: Vec<MarkdownNoteSummary>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
/// Legacy bounded owned note result, including optional recovered text and its disk baseline.
pub struct OpenMarkdownNote {
    pub relative_path: String,
    pub title: String,
    pub content: String,
    pub revision: String,
    pub byte_count: u64,
    pub character_count: usize,
    pub line_count: usize,
    pub line_separator: &'static str,
    pub modified_at_ms: i64,
    pub recovered_conflict: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Legacy display metrics computed in the same validated read as the note body.
struct MarkdownOpenMetrics {
    character_count: usize,
    line_count: usize,
    line_separator: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
/// Exact persisted revision and filesystem metadata after a completed save boundary.
pub struct MarkdownSaveReceipt {
    pub revision: String,
    pub byte_count: u64,
    pub modified_at_ms: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
/// New authoritative identifier and revision after an explicit note lifecycle operation.
pub struct MarkdownNoteMutationReceipt {
    pub relative_path: String,
    pub title: String,
    pub revision: String,
    pub byte_count: u64,
    pub modified_at_ms: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
/// Imported asset identity and safe insertion text; the caller still owns its editor transaction.
pub struct MarkdownAttachmentImportReceipt {
    pub attachment_relative_path: String,
    pub markdown_insertion: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
/// Compact search output with at most one bounded body snippet per matching note.
pub struct MarkdownSearchResult {
    pub relative_path: String,
    pub title: String,
    pub snippet: Option<String>,
}

/// Configured session authority; paths and atomic counters own no idle filesystem or worker resources.
pub struct MarkdownWorkspace {
    configured_root: PathBuf,
    conflict_recovery_directory: PathBuf,
    active_root: Mutex<Option<PathBuf>>,
    search_generation: Arc<AtomicU64>,
    /// 目录刷新独立于搜索；文件集合/排序身份变化与会话结束撤销目录权限，同路径正文保存保留。
    browse_generation: Arc<AtomicU64>,
    save_sequence: Arc<AtomicU64>,
}

#[derive(Clone)]
/// Lightweight active-session access; its owner must cancel and join any caller-created workers.
pub struct MarkdownSessionAccess {
    canonical_root: PathBuf,
    conflict_recovery_directory: PathBuf,
    search_generation: Arc<AtomicU64>,
    /// 克隆会话只共享撤权计数，不持目录句柄或目录索引。
    browse_generation: Arc<AtomicU64>,
    save_sequence: Arc<AtomicU64>,
}

impl MarkdownWorkspace {
    #[must_use]
    /// Configure vault and recovery paths without creating directories or activating the plugin.
    pub fn new(configured_root: PathBuf, app_data_directory: PathBuf) -> Self {
        Self::with_storage_locations(
            configured_root,
            app_data_directory.join(CONFLICT_RECOVERY_DIRECTORY_NAME),
        )
    }

    #[cfg(test)]
    /// Create isolated recovery storage beneath a test vault without touching application data.
    fn with_root(configured_root: PathBuf) -> Self {
        let recovery_directory = configured_root.join(".omni-md-test-conflict-recovery");
        Self::with_storage_locations(configured_root, recovery_directory)
    }

    /// Initialize path and generation ownership without opening files or starting background work.
    fn with_storage_locations(
        configured_root: PathBuf,
        conflict_recovery_directory: PathBuf,
    ) -> Self {
        Self {
            configured_root,
            conflict_recovery_directory,
            active_root: Mutex::new(None),
            search_generation: Arc::new(AtomicU64::new(0)),
            browse_generation: Arc::new(AtomicU64::new(0)),
            save_sequence: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Establish the canonical vault and invalidate work from any previous activation.
    pub fn open_session(&self) -> Result<MarkdownSessionAccess, MarkdownWorkspaceError> {
        ensure_workspace_root(&self.configured_root)?;
        let canonical_root = self.configured_root.canonicalize().map_err(|_| {
            // An unavailable root cannot become the stable canonical session boundary.
            MarkdownWorkspaceError::RootUnavailable
        })?;
        if !canonical_root.is_dir() {
            // A root that changed into a non-directory cannot be published as an active session.
            return Err(MarkdownWorkspaceError::RootUnavailable);
        }

        // Publish only canonical activation state, then supersede searches from an older activation.
        *self.active_root.lock().map_err(|_| {
            // A poisoned activation lock cannot safely publish or recover a session handle.
            MarkdownWorkspaceError::SessionUnavailable
        })? = Some(canonical_root.clone());
        self.search_generation.fetch_add(1, AtomicOrdering::AcqRel);
        self.browse_generation.fetch_add(1, AtomicOrdering::AcqRel);
        Ok(self.session_access(canonical_root))
    }

    /// Clone lightweight access to the currently activated canonical vault or report a closed session.
    pub fn active_session(&self) -> Result<MarkdownSessionAccess, MarkdownWorkspaceError> {
        let canonical_root = self
            .active_root
            .lock()
            .map_err(|_| {
                // A poisoned activation lock cannot safely publish or recover a session handle.
                MarkdownWorkspaceError::SessionUnavailable
            })?
            .clone()
            .ok_or(MarkdownWorkspaceError::SessionClosed)?;
        Ok(self.session_access(canonical_root))
    }

    /// Invalidate in-flight searches before clearing the workspace activation marker.
    pub fn close_session(&self) {
        self.search_generation.fetch_add(1, AtomicOrdering::AcqRel);
        self.browse_generation.fetch_add(1, AtomicOrdering::AcqRel);
        if let Ok(mut active_root) = self.active_root.lock() {
            // A nonpoisoned lock permits immediate marker release; no filesystem resources are owned by this marker.
            active_root.take();
        }
    }

    #[cfg(test)]
    /// Inspect the activation marker for lifecycle regression tests without reopening the vault.
    fn is_session_open(&self) -> bool {
        self.active_root.lock().is_ok_and(|active_root| {
            // Inspect only activation ownership for lifecycle assertions without opening the vault.
            active_root.is_some()
        })
    }

    /// Share only canonical paths and atomic sequences with an explicitly active caller.
    fn session_access(&self, canonical_root: PathBuf) -> MarkdownSessionAccess {
        MarkdownSessionAccess {
            canonical_root,
            conflict_recovery_directory: self.conflict_recovery_directory.clone(),
            search_generation: Arc::clone(&self.search_generation),
            browse_generation: Arc::clone(&self.browse_generation),
            save_sequence: Arc::clone(&self.save_sequence),
        }
    }
}

impl MarkdownSessionAccess {
    /// Opens the active native editor model through the same canonical vault
    /// confinement used by every other note operation. The buffer repeats the
    /// final O_NOFOLLOW/regular-file checks at descriptor-open time so a path
    /// replacement race cannot turn validation into a symlink traversal.
    #[cfg(feature = "native-markdown-candidate")]
    /// Open the native document through vault confinement and descriptor-time validation.
    pub fn open_document_buffer(
        &self,
        relative_path: &str,
    ) -> Result<MarkdownDocumentBuffer, MarkdownWorkspaceError> {
        let path = resolve_existing_markdown(&self.canonical_root, relative_path)?;
        let relative_path = relative_markdown_path(&self.canonical_root, &path)?;
        if let Some(document) = self.open_pending_legacy_recovery(&path, &relative_path)? {
            // 旧record正文直接由独立snapshot提供；其未知base保持pending，不能自动覆盖当前磁盘。
            return Ok(document);
        }
        MarkdownDocumentBuffer::open(&path, MAX_DOCUMENT_BYTES)
            .map_err(markdown_document_buffer_open_error)
    }

    /// Enumerate bounded sidebar metadata without reading note bodies or retaining an index.
    /// `requested_limit` caps returned entries; `scanned_entry_limit` also counts
    /// ignored directory entries so unrelated files cannot bypass the work budget.
    #[cfg(feature = "native-markdown-candidate")]
    /// Collect bounded sidebar metadata while charging every visited entry and reading no note bodies.
    pub fn list_workspace_bounded(
        &self,
        requested_limit: usize,
        scanned_entry_limit: usize,
    ) -> Result<MarkdownWorkspaceSnapshot, MarkdownWorkspaceError> {
        if requested_limit == 0 || scanned_entry_limit == 0 {
            // A zero output or scan budget returns immediately without filesystem traversal.
            return Ok(MarkdownWorkspaceSnapshot {
                directories: Vec::new(),
                notes: Vec::new(),
            });
        }

        let mut directories = Vec::with_capacity(requested_limit.min(16));
        let mut notes = Vec::with_capacity(requested_limit);
        walk_markdown_workspace_bounded(&self.canonical_root, scanned_entry_limit, |entry| {
            // Collect metadata synchronously and signal stop as soon as the combined output budget is full.
            match entry {
                // Directory callbacks retain only vault-relative identifiers, never open directory handles.
                MarkdownFilesystemEntry::Directory(path) => {
                    directories.push(relative_workspace_path(&self.canonical_root, path)?);
                }
                // Note callbacks collect metadata only; document bytes remain unopened.
                MarkdownFilesystemEntry::Note(path) => {
                    let metadata = path.metadata().map_err(|_| {
                        // Translate a failed filesystem stage into the stable vault-storage error category.
                        MarkdownWorkspaceError::StorageUnavailable
                    })?;
                    notes.push(MarkdownNoteSummary {
                        relative_path: relative_markdown_path(&self.canonical_root, path)?,
                        title: markdown_title(path)?,
                        byte_count: metadata.len(),
                        modified_at_ms: modified_at_ms(&metadata),
                    });
                }
            }
            Ok(if directories.len() + notes.len() >= requested_limit {
                // The combined output count has reached the requested limit, so end traversal now.
                WalkControl::Stop
            } else {
                // More capacity remains and the walker may inspect another charged entry.
                WalkControl::Continue
            })
        })?;
        directories.sort_unstable();
        notes.sort_unstable_by(|left, right| {
            // Keep sidebar note order deterministic across filesystem enumeration orders.
            left.relative_path.cmp(&right.relative_path)
        });
        Ok(MarkdownWorkspaceSnapshot { directories, notes })
    }

    /// Collect and sort vault metadata for the legacy complete-listing API.
    pub fn list_workspace(&self) -> Result<MarkdownWorkspaceSnapshot, MarkdownWorkspaceError> {
        let mut directories = Vec::new();
        let mut notes = Vec::new();
        walk_markdown_workspace(&self.canonical_root, |entry| {
            // Collect only relative directory identifiers and compact note metadata from each borrowed traversal entry.
            match entry {
                // Expose user directories as vault-relative identifiers.
                MarkdownFilesystemEntry::Directory(path) => {
                    directories.push(relative_workspace_path(&self.canonical_root, path)?);
                }
                // Expose regular Markdown files through compact metadata summaries.
                MarkdownFilesystemEntry::Note(path) => {
                    let metadata = path.metadata().map_err(|_| {
                        // Translate a failed filesystem stage into the stable vault-storage error category.
                        MarkdownWorkspaceError::StorageUnavailable
                    })?;
                    notes.push(MarkdownNoteSummary {
                        relative_path: relative_markdown_path(&self.canonical_root, path)?,
                        title: markdown_title(path)?,
                        byte_count: metadata.len(),
                        modified_at_ms: modified_at_ms(&metadata),
                    });
                }
            }
            Ok(WalkControl::Continue)
        })?;
        directories.sort_unstable();
        notes.sort_unstable_by(|left, right| {
            // Keep sidebar note order deterministic across filesystem enumeration orders.
            left.relative_path.cmp(&right.relative_path)
        });
        Ok(MarkdownWorkspaceSnapshot { directories, notes })
    }

    /// Open the legacy String-backed note and overlay a validated conflict draft when present.
    pub fn open_note(
        &self,
        relative_path: &str,
    ) -> Result<OpenMarkdownNote, MarkdownWorkspaceError> {
        let mut note = self.open_disk_note(relative_path)?;
        if let Some(recovered_content) =
            read_conflict_draft(&self.conflict_recovery_directory, &note.relative_path)?
        {
            // A valid private draft supplies unsaved text while retaining the disk revision as its conflict baseline.
            let recovered_metrics = markdown_open_metrics(&recovered_content);
            note.byte_count = recovered_content.len() as u64;
            note.character_count = recovered_metrics.character_count;
            note.line_count = recovered_metrics.line_count;
            note.line_separator = recovered_metrics.line_separator;
            note.content = recovered_content;
            note.recovered_conflict = true;
        }
        Ok(note)
    }

    /// Explicitly choose the disk version and clear its legacy recovery draft only after a successful read.
    pub fn reload_note_from_disk(
        &self,
        relative_path: &str,
    ) -> Result<OpenMarkdownNote, MarkdownWorkspaceError> {
        let note = self.open_disk_note(relative_path)?;
        remove_conflict_draft(&self.conflict_recovery_directory, &note.relative_path)?;
        Ok(note)
    }

    /// Persist a bounded legacy draft without requiring the original note to still exist.
    pub fn update_conflict_draft(
        &self,
        relative_path: &str,
        content: &str,
    ) -> Result<(), MarkdownWorkspaceError> {
        validate_conflict_draft_relative_path(relative_path)?;
        if content.len() as u64 > MAX_DOCUMENT_BYTES {
            // Reject oversized unsaved content before touching its previous recovery record.
            return Err(MarkdownWorkspaceError::DocumentTooLarge);
        }
        write_conflict_draft(
            &self.conflict_recovery_directory,
            relative_path,
            content,
            &self.save_sequence,
        )
    }

    /// Explicitly discard the validated legacy recovery record for one note identifier.
    pub fn clear_conflict_draft(&self, relative_path: &str) -> Result<(), MarkdownWorkspaceError> {
        validate_conflict_draft_relative_path(relative_path)?;
        remove_conflict_draft(&self.conflict_recovery_directory, relative_path)
    }

    /// Validate and inspect one disk note, producing the legacy owned text and its exact revision.
    fn open_disk_note(
        &self,
        relative_path: &str,
    ) -> Result<OpenMarkdownNote, MarkdownWorkspaceError> {
        let path = resolve_existing_markdown(&self.canonical_root, relative_path)?;
        let relative_path = relative_markdown_path(&self.canonical_root, &path)?;
        let title = markdown_title(&path)?;
        let mapped = inspect_mapped_markdown(&path, MAX_DOCUMENT_BYTES, |text| {
            // The legacy API deliberately copies one bounded current note and derives display metrics from the same validated bytes.
            let metrics = markdown_open_metrics(text);
            (text.to_owned(), metrics)
        })?;
        let (content, metrics) = mapped.value;
        Ok(OpenMarkdownNote {
            relative_path,
            title,
            content,
            revision: mapped.revision,
            byte_count: mapped.byte_count,
            character_count: metrics.character_count,
            line_count: metrics.line_count,
            line_separator: metrics.line_separator,
            modified_at_ms: mapped.modified_at_ms,
            recovered_conflict: false,
        })
    }

    /// Create a heading note with bounded collision retries and exclusive file creation.
    pub fn create_note(
        &self,
        relative_directory: &str,
        requested_title: &str,
    ) -> Result<OpenMarkdownNote, MarkdownWorkspaceError> {
        // 公开入口始终使用真实目录sync；故障注入仅由同模块私有测试调用。
        self.create_note_with_directory_sync(relative_directory, requested_title, sync_directory)
    }

    /// 同一次结构操作的私有目录持久化边界，不新增公开flag或运行时重试。
    fn create_note_with_directory_sync(
        &self,
        relative_directory: &str,
        requested_title: &str,
        mut synchronize_directory: impl FnMut(&Path) -> Result<(), MarkdownWorkspaceError>,
    ) -> Result<OpenMarkdownNote, MarkdownWorkspaceError> {
        // 先声明撤权边界，所有后建文件/rollback guard先drop；纯预校验不激活。
        let publication = WorkspaceStructurePublication::new(self);
        let result = (|| {
            let directory = resolve_existing_directory(&self.canonical_root, relative_directory)?;
            let title = validated_note_title(requested_title)?;

            let mut created_path = None;
            for suffix in 1_u16..=1_000 {
                // Each candidate is attempted exclusively; occupied names never overwrite existing notes.
                let file_name = if suffix == 1 {
                    // Try the requested unsuffixed title first.
                    format!("{title}.md")
                } else {
                    // Later candidates append a human-readable bounded numeric suffix.
                    format!("{title} {suffix}.md")
                };
                let candidate = directory.join(file_name);
                match create_new_markdown_file(
                    &publication,
                    &candidate,
                    format!("# {title}\n").as_bytes(),
                ) {
                    // Exclusive creation succeeded; retain the authoritative path and stop trying names.
                    Ok(_acquired) => {
                        created_path = Some(candidate);
                        break;
                    }
                    // An occupied candidate remains unchanged while the next suffix is attempted.
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    // Other filesystem errors abort creation rather than masquerading as name collisions.
                    Err(_) => return Err(MarkdownWorkspaceError::StorageUnavailable),
                }
            }
            let path = created_path.ok_or(MarkdownWorkspaceError::NameCollisionLimitExceeded)?;
            // 新笔记已真实发布；后续sync/open失败也不能让旧搜索末项继续代表原目录。
            publication.changed();
            synchronize_directory(&directory)?;
            self.open_note(&relative_markdown_path(&self.canonical_root, &path)?)
        })();
        // 闭包内的pending/inspect guard此时已全部析构，不能提前取未确认标量。
        publication.finish(result)
    }

    /// Create one validated user directory and durably publish its vault-relative identifier.
    pub fn create_directory(
        &self,
        relative_parent: &str,
        requested_name: &str,
    ) -> Result<String, MarkdownWorkspaceError> {
        // 公开入口始终使用真实目录sync；故障注入仅由同模块私有测试调用。
        self.create_directory_with_directory_sync(relative_parent, requested_name, sync_directory)
    }

    /// 同一次结构操作的私有目录持久化边界，不新增公开flag或运行时重试。
    fn create_directory_with_directory_sync(
        &self,
        relative_parent: &str,
        requested_name: &str,
        mut synchronize_directory: impl FnMut(&Path) -> Result<(), MarkdownWorkspaceError>,
    ) -> Result<String, MarkdownWorkspaceError> {
        // 先声明撤权边界，所有后建文件/rollback guard先drop；纯预校验不激活。
        let publication = WorkspaceStructurePublication::new(self);
        let result = (|| {
            let parent = resolve_existing_directory(&self.canonical_root, relative_parent)?;
            let name = validate_entry_name(
                requested_name,
                MAX_DIRECTORY_NAME_CHARACTERS,
                MarkdownWorkspaceError::InvalidDirectoryName,
            )?;
            if parent == self.canonical_root
                && name.eq_ignore_ascii_case(ATTACHMENTS_DIRECTORY_NAME)
            {
                // The root asset directory name is reserved for attachment storage.
                return Err(MarkdownWorkspaceError::ReservedDirectoryName);
            }
            let directory = parent.join(name);
            fs::create_dir(&directory).map_err(|error| {
                // Keep destination collisions distinct from other filesystem failures so existing entries are never silently replaced.
                match error.kind() {
                    // An existing destination is a caller-visible name collision.
                    std::io::ErrorKind::AlreadyExists => MarkdownWorkspaceError::EntryAlreadyExists,
                    // Other creation failures indicate unavailable storage.
                    _ => MarkdownWorkspaceError::StorageUnavailable,
                }
            })?;
            // 真实目录已创建，撤销旧查询遍历；失败的目录sync不会恢复旧搜索权限。
            publication.changed();
            synchronize_directory(&parent)?;
            let canonical_directory = directory.canonicalize().map_err(|_| {
                // Translate a failed filesystem stage into the stable vault-storage error category.
                MarkdownWorkspaceError::StorageUnavailable
            })?;
            ensure_within_root(&self.canonical_root, &canonical_directory)?;
            relative_workspace_path(&self.canonical_root, &canonical_directory)
        })();
        // 闭包内的pending/inspect guard此时已全部析构，不能提前取未确认标量。
        publication.finish(result)
    }

    /// Rename a user directory without replacing existing entries or moving the vault root.
    pub fn rename_directory(
        &self,
        relative_path: &str,
        requested_name: &str,
    ) -> Result<String, MarkdownWorkspaceError> {
        // 公开入口始终使用真实目录sync；故障注入仅由同模块私有测试调用。
        self.rename_directory_with_directory_sync(relative_path, requested_name, sync_directory)
    }

    /// 同一次结构操作的私有目录持久化边界，不新增公开flag或运行时重试。
    fn rename_directory_with_directory_sync(
        &self,
        relative_path: &str,
        requested_name: &str,
        mut synchronize_directory: impl FnMut(&Path) -> Result<(), MarkdownWorkspaceError>,
    ) -> Result<String, MarkdownWorkspaceError> {
        // 先声明撤权边界，所有后建文件/rollback guard先drop；纯预校验不激活。
        let publication = WorkspaceStructurePublication::new(self);
        let result = (|| {
            let source = resolve_existing_directory(&self.canonical_root, relative_path)?;
            if source == self.canonical_root {
                // The canonical root is a session boundary and cannot be renamed through a child operation.
                return Err(MarkdownWorkspaceError::RootDirectoryProtected);
            }
            let parent = source
                .parent()
                .ok_or(MarkdownWorkspaceError::InvalidRelativePath)?;
            ensure_within_root(&self.canonical_root, parent)?;
            let name = validate_entry_name(
                requested_name,
                MAX_DIRECTORY_NAME_CHARACTERS,
                MarkdownWorkspaceError::InvalidDirectoryName,
            )?;
            if parent == self.canonical_root
                && name.eq_ignore_ascii_case(ATTACHMENTS_DIRECTORY_NAME)
            {
                // A root child cannot take the reserved attachment directory name.
                return Err(MarkdownWorkspaceError::ReservedDirectoryName);
            }
            let destination = parent.join(name);
            if destination == source {
                // An unchanged destination returns the same identifier without filesystem mutation.
                return relative_workspace_path(&self.canonical_root, &source);
            }

            rename_without_replacement(&source, &destination).map_err(|error| {
                // Keep destination collisions distinct from other filesystem failures so existing entries are never silently replaced.
                match error.kind() {
                    // An occupied destination must remain untouched.
                    std::io::ErrorKind::AlreadyExists => MarkdownWorkspaceError::EntryAlreadyExists,
                    // Other rename failures abort with a storage error.
                    _ => MarkdownWorkspaceError::StorageUnavailable,
                }
            })?;
            // 名称已经真实移动，先撤权再执行可能失败的目录持久化。
            publication.changed();
            synchronize_directory(parent)?;
            publication.changed();
            relative_workspace_path(&self.canonical_root, &destination)
        })();
        // 闭包内的pending/inspect guard此时已全部析构，不能提前取未确认标量。
        publication.finish(result)
    }

    /// Delete an explicitly selected user subtree while protecting the vault root.
    pub fn delete_directory(&self, relative_path: &str) -> Result<(), MarkdownWorkspaceError> {
        // 公开入口始终使用真实目录sync；故障注入仅由同模块私有测试调用。
        self.delete_directory_with_directory_sync(relative_path, sync_directory)
    }

    /// 同一次结构操作的私有目录持久化边界，不新增公开flag或运行时重试。
    fn delete_directory_with_directory_sync(
        &self,
        relative_path: &str,
        synchronize_directory: impl FnMut(&Path) -> Result<(), MarkdownWorkspaceError>,
    ) -> Result<(), MarkdownWorkspaceError> {
        self.delete_directory_with_removal(relative_path, synchronize_directory, |path| {
            fs::remove_dir_all(path)
        })
    }

    /// 保留std安全递归，私有移除参数仅供真实部分删除后的失败反例。
    fn delete_directory_with_removal(
        &self,
        relative_path: &str,
        mut synchronize_directory: impl FnMut(&Path) -> Result<(), MarkdownWorkspaceError>,
        remove_directory: impl FnOnce(&Path) -> std::io::Result<()>,
    ) -> Result<(), MarkdownWorkspaceError> {
        // 先声明撤权边界，所有后建文件/rollback guard先drop；纯预校验不激活。
        let publication = WorkspaceStructurePublication::new(self);
        let result = (|| {
            let directory = resolve_existing_directory(&self.canonical_root, relative_path)?;
            if directory == self.canonical_root {
                // The root cannot be recursively removed through the directory deletion API.
                return Err(MarkdownWorkspaceError::RootDirectoryProtected);
            }
            let parent = directory
                .parent()
                .ok_or(MarkdownWorkspaceError::InvalidRelativePath)?;
            ensure_within_root(&self.canonical_root, parent)?;
            // opaque std递归可能部分成功后Err，纯root保护之后保守封闭旧扫描。
            publication.recursive_delete_attempt();
            remove_directory(&directory).map_err(|_| {
                // Translate a failed filesystem stage into the stable vault-storage error category.
                MarkdownWorkspaceError::StorageUnavailable
            })?;
            synchronize_directory(parent)?;
            publication.changed();
            Ok(())
        })();
        // 闭包内的pending/inspect guard此时已全部析构，不能提前取未确认标量。
        publication.finish(result)
    }

    /// Return a confined canonical directory for an explicitly requested platform action.
    pub fn resolve_directory_for_system_action(
        &self,
        relative_path: &str,
    ) -> Result<PathBuf, MarkdownWorkspaceError> {
        resolve_existing_directory(&self.canonical_root, relative_path)
    }

    /// Copy a bounded validated external Markdown file into an exclusive vault destination.
    pub fn import_note(
        &self,
        source_path: &Path,
        destination_directory: &str,
    ) -> Result<MarkdownNoteMutationReceipt, MarkdownWorkspaceError> {
        // 公开入口始终使用真实目录sync；故障注入仅由同模块私有测试调用。
        self.import_note_with_directory_sync(source_path, destination_directory, sync_directory)
    }

    /// 同一次结构操作的私有目录持久化边界，不新增公开flag或运行时重试。
    fn import_note_with_directory_sync(
        &self,
        source_path: &Path,
        destination_directory: &str,
        mut synchronize_directory: impl FnMut(&Path) -> Result<(), MarkdownWorkspaceError>,
    ) -> Result<MarkdownNoteMutationReceipt, MarkdownWorkspaceError> {
        // 先声明撤权边界，所有后建文件/rollback guard先drop；纯预校验不激活。
        let publication = WorkspaceStructurePublication::new(self);
        let result = (|| {
            validate_external_import_source(source_path)?;
            if !has_markdown_extension(source_path) {
                // An external file without the supported suffix cannot enter the note API.
                return Err(MarkdownWorkspaceError::NotMarkdown);
            }
            let source_title = markdown_title(source_path)?;
            let source_title = validate_entry_name(
                &source_title,
                MAX_NOTE_TITLE_CHARACTERS,
                MarkdownWorkspaceError::InvalidNoteTitle,
            )?;
            let destination_parent =
                resolve_existing_directory(&self.canonical_root, destination_directory)?;
            let inspected = inspect_bounded_markdown(source_path, MAX_DOCUMENT_BYTES, |content| {
                // 只从同一个已验证冻结源分块写入；关闭/路径复核失败时完整目标仍由guard回滚。
                // 目标写入结果保留在内层；外层只把源读取失败转换为导入源错误。
                Ok(create_collision_safe_imported_note_with_writer(
                    &publication,
                    &destination_parent,
                    source_title,
                    content.len() as u64,
                    |file| content.copy_to(file).map_err(std::io::Error::other),
                ))
            })
            .map_err(markdown_import_read_error)?;
            let mut imported = inspected.value?;
            synchronize_directory(&destination_parent)?;
            imported.verify_for_commit()?;
            let receipt =
                mutation_receipt(&self.canonical_root, &imported.path, inspected.revision)?;
            imported.disarm();
            publication.changed();
            Ok(receipt)
        })();
        // 闭包内的pending/inspect guard此时已全部析构，不能提前取未确认标量。
        publication.finish(result)
    }

    /// Stream an external attachment into private vault ownership and return safe insertion Markdown.
    pub fn import_attachment(
        &self,
        note_relative_path: &str,
        source_path: &Path,
    ) -> Result<MarkdownAttachmentImportReceipt, MarkdownWorkspaceError> {
        // Legacy explicit import accepts the asset independently of a native editor transaction.
        Ok(self
            .prepare_attachment_import(note_relative_path, source_path)?
            .commit())
    }

    /// Copy an asset under rollback ownership until the caller accepts its canonical link edit.
    ///
    /// The pending token owns transient descriptors, not document text. Commit it after successful
    /// insertion, or explicitly roll it back to observe cleanup errors. Dropping it attempts bounded
    /// identity-checked cleanup and preserves any observed external replacement or modification.
    pub fn prepare_attachment_import(
        &self,
        note_relative_path: &str,
        source_path: &Path,
    ) -> Result<MarkdownPendingAttachmentImport, MarkdownWorkspaceError> {
        let note = resolve_existing_markdown(&self.canonical_root, note_relative_path)?;
        validate_external_import_source(source_path)?;
        let source_name = source_path
            .file_name()
            .and_then(|name| {
                // Only UTF-8 names can be safely exposed and reproduced by the public path API.
                name.to_str()
            })
            .ok_or(MarkdownWorkspaceError::UnsupportedFileName)?;
        let source_name = validate_entry_name(
            source_name,
            MAX_ATTACHMENT_NAME_CHARACTERS,
            MarkdownWorkspaceError::InvalidAttachmentName,
        )?;
        let mut source = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(source_path)
            .map_err(|_| {
                // A failed external-source read or lookup must not be reported as a successfully imported file.
                MarkdownWorkspaceError::ImportSourceUnavailable
            })?;
        let before = source.metadata().map_err(|_| {
            // A failed external-source read or lookup must not be reported as a successfully imported file.
            MarkdownWorkspaceError::ImportSourceUnavailable
        })?;
        if !before.is_file() {
            // The opened source descriptor must be a regular file rather than a device or directory.
            return Err(MarkdownWorkspaceError::ImportSourceUnavailable);
        }
        if before.len() > MAX_ATTACHMENT_BYTES {
            // Reject oversized assets before creating their vault destination.
            return Err(MarkdownWorkspaceError::AttachmentTooLarge);
        }
        // A fixed source extent and identity bound the copy and detect concurrent source changes.
        let identity = FileIdentity::from(&before);
        let attachments_directory = ensure_attachments_directory(&self.canonical_root)?;

        let imported = create_collision_safe_attachment(
            &self.canonical_root,
            &attachments_directory,
            source_name,
            &mut source,
            before.len(),
        )?;
        let after = source.metadata().map_err(|_| {
            // A failed identity recheck cannot establish that inspected or copied source bytes stayed stable.
            MarkdownWorkspaceError::DocumentChangedDuringRead
        })?;
        let path_after = fs::symlink_metadata(source_path).map_err(|_| {
            // A failed identity recheck cannot establish that inspected or copied source bytes stayed stable.
            MarkdownWorkspaceError::DocumentChangedDuringRead
        })?;
        if path_after.file_type().is_symlink()
            || FileIdentity::from(&after) != identity
            || FileIdentity::from(&path_after) != identity
        {
            // A source identity change invalidates the copy; the armed destination guard removes uncommitted bytes.
            return Err(MarkdownWorkspaceError::DocumentChangedDuringRead);
        }
        sync_directory(&attachments_directory)?;

        let attachment_relative_path =
            relative_workspace_path(&self.canonical_root, &imported.path)?;
        let note_relative_path = relative_markdown_path(&self.canonical_root, &note)?;
        let markdown_insertion = markdown_attachment_insertion(
            &note_relative_path,
            imported
                .path
                .file_name()
                .and_then(|name| {
                    // Only UTF-8 names can be safely exposed and reproduced by the public path API.
                    name.to_str()
                })
                .ok_or(MarkdownWorkspaceError::UnsupportedFileName)?,
        )?;
        // Receipt construction preserves rollback ownership until the native canonical insertion succeeds.
        let receipt = MarkdownAttachmentImportReceipt {
            attachment_relative_path,
            markdown_insertion,
        };
        // Recheck the exact acquired inode and directory after constructing all public insertion metadata.
        imported.verify_current()?;
        Ok(MarkdownPendingAttachmentImport::new(receipt, imported))
    }

    /// Rename a revision-checked note through a nonoverwriting same-directory operation.
    pub fn rename_note(
        &self,
        relative_path: &str,
        requested_title: &str,
        expected_revision: Option<&str>,
    ) -> Result<MarkdownNoteMutationReceipt, MarkdownWorkspaceError> {
        // 公开入口始终使用真实目录sync；故障注入仅由同模块私有测试调用。
        self.rename_note_with_directory_sync(
            relative_path,
            requested_title,
            expected_revision,
            sync_directory,
        )
    }

    /// 同一次结构操作的私有目录持久化边界，不新增公开flag或运行时重试。
    fn rename_note_with_directory_sync(
        &self,
        relative_path: &str,
        requested_title: &str,
        expected_revision: Option<&str>,
        mut synchronize_directory: impl FnMut(&Path) -> Result<(), MarkdownWorkspaceError>,
    ) -> Result<MarkdownNoteMutationReceipt, MarkdownWorkspaceError> {
        // 先声明撤权边界，所有后建文件/rollback guard先drop；纯预校验不激活。
        let publication = WorkspaceStructurePublication::new(self);
        let result = (|| {
            let source = resolve_existing_markdown(&self.canonical_root, relative_path)?;
            let title = validated_note_title(requested_title)?;
            let parent = source
                .parent()
                .ok_or(MarkdownWorkspaceError::InvalidRelativePath)?;
            ensure_within_root(&self.canonical_root, parent)?;
            let destination = parent.join(format!("{title}.md"));

            let current = inspect_bounded_markdown(&source, MAX_DOCUMENT_BYTES, |_| {
                // 摘要同样通过有限UTF8/SHA扫描，不能因callback不读正文就先整幅触页。
                Ok(())
            })?;
            ensure_expected_revision(&current.revision, expected_revision)?;
            if destination == source {
                // An unchanged title requires no rename and can return the existing receipt.
                return mutation_receipt(&self.canonical_root, &source, current.revision);
            }

            rename_without_replacement(&source, &destination).map_err(|error| {
                // Keep destination collisions distinct from other filesystem failures so existing entries are never silently replaced.
                match error.kind() {
                    // A destination collision must not replace another note.
                    std::io::ErrorKind::AlreadyExists => MarkdownWorkspaceError::EntryAlreadyExists,
                    // Other rename failures prevent publishing a new note identifier.
                    _ => MarkdownWorkspaceError::StorageUnavailable,
                }
            })?;
            // 名称已经真实移动，先撤权再执行可能失败的目录持久化。
            publication.changed();
            synchronize_directory(parent)?;
            publication.changed();
            mutation_receipt(&self.canonical_root, &destination, current.revision)
        })();
        // 闭包内的pending/inspect guard此时已全部析构，不能提前取未确认标量。
        publication.finish(result)
    }

    /// Move a revision-checked note and rewrite only its imported attachment destinations when necessary.
    pub fn move_note(
        &self,
        relative_path: &str,
        destination_directory: &str,
        expected_revision: Option<&str>,
    ) -> Result<MarkdownNoteMutationReceipt, MarkdownWorkspaceError> {
        // 公开入口始终使用真实目录sync；故障注入仅由同模块私有测试调用。
        self.move_note_with_directory_sync(
            relative_path,
            destination_directory,
            expected_revision,
            sync_directory,
        )
    }

    /// 同一次结构操作的私有目录持久化边界，不新增公开flag或运行时重试。
    fn move_note_with_directory_sync(
        &self,
        relative_path: &str,
        destination_directory: &str,
        expected_revision: Option<&str>,
        mut synchronize_directory: impl FnMut(&Path) -> Result<(), MarkdownWorkspaceError>,
    ) -> Result<MarkdownNoteMutationReceipt, MarkdownWorkspaceError> {
        // 先声明撤权边界，所有后建文件/rollback guard先drop；纯预校验不激活。
        let publication = WorkspaceStructurePublication::new(self);
        let result = (|| {
            let source = resolve_existing_markdown(&self.canonical_root, relative_path)?;
            let source_parent = source
                .parent()
                .ok_or(MarkdownWorkspaceError::InvalidRelativePath)?;
            let destination_parent =
                resolve_existing_directory(&self.canonical_root, destination_directory)?;
            let file_name = source
                .file_name()
                .ok_or(MarkdownWorkspaceError::UnsupportedFileName)?;
            let destination = destination_parent.join(file_name);

            let source_relative_path = relative_markdown_path(&self.canonical_root, &source)?;
            let destination_relative_path =
                relative_markdown_path(&self.canonical_root, &destination)?;
            let current = inspect_bounded_markdown(&source, MAX_DOCUMENT_BYTES, |content| {
                // 只把真实需要替换的附件前缀流式写入匿名spool；超长行/URL不进入整篇String。
                rewrite_moved_note_attachment_links_streamed(
                    content,
                    &source_relative_path,
                    &destination_relative_path,
                )
            })?;
            ensure_expected_revision(&current.revision, expected_revision)?;
            if destination == source {
                // A move to the same parent keeps the original bytes and identity.
                return mutation_receipt(&self.canonical_root, &source, current.revision);
            }

            let Some(mut rewritten) = current.value else {
                // Unchanged attachment prefixes permit a direct exclusive rename with no rewritten body.
                rename_without_replacement(&source, &destination).map_err(|error| {
                    // Preserve collision classification from an exclusive rename while keeping all other platform failures as storage errors.
                    match error.kind() {
                        // An occupied plain-move destination is preserved.
                        std::io::ErrorKind::AlreadyExists => {
                            MarkdownWorkspaceError::EntryAlreadyExists
                        }
                        // Other plain-move errors leave the original in place.
                        _ => MarkdownWorkspaceError::StorageUnavailable,
                    }
                })?;
                // plain rename已改变路径，所有后续持久化之前先双撤。
                publication.changed();
                synchronize_directory(source_parent)?;
                if destination_parent != source_parent {
                    // A cross-directory rename changes metadata in both parents, so sync both.
                    synchronize_directory(&destination_parent)?;
                }
                publication.changed();
                return mutation_receipt(&self.canonical_root, &destination, current.revision);
            };

            // A depth-changing move creates guarded rewritten bytes before removing the revision-checked original.
            let source_permissions = source
                .metadata()
                .map_err(|_| {
                    // Translate a failed filesystem stage into the stable vault-storage error category.
                    MarkdownWorkspaceError::StorageUnavailable
                })?
                .permissions();
            let acquired_destination = create_new_markdown_file_from_stream(
                &publication,
                &destination,
                rewritten.byte_len,
                Some(source_permissions),
                |file| {
                    // spool由本次原冻结源独占且已定位0；通过流式/内核复制转移，不向应用返回整篇正文。
                    let copied = std::io::copy(&mut rewritten.file, file)?;
                    if copied != rewritten.byte_len {
                        return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
                    }
                    Ok(())
                },
            )
            .map_err(|error| {
                // Keep destination collisions distinct from other filesystem failures so existing entries are never silently replaced.
                match error.kind() {
                    // A rewritten move must not replace an existing destination.
                    std::io::ErrorKind::AlreadyExists => MarkdownWorkspaceError::EntryAlreadyExists,
                    // Other destination creation failures abort before the original is removed.
                    _ => MarkdownWorkspaceError::StorageUnavailable,
                }
            })?;
            let mut pending_destination =
                PendingVaultFile::new(destination.clone(), acquired_destination, &publication);
            synchronize_directory(&destination_parent)?;
            let receipt = mutation_receipt(&self.canonical_root, &destination, rewritten.revision)?;

            // Recheck the original before removal; destination rollback remains armed until removal succeeds.
            let rechecked = inspect_bounded_markdown(&source, MAX_DOCUMENT_BYTES, |_| {
                // 删除原文件前仍核同一完整摘要；实际读取和页面归还均有界。
                Ok(())
            })?;
            ensure_expected_revision(&rechecked.revision, expected_revision)?;
            // 调用者未提供版本也必须比较本次改写的原始版本，避免删除同步期间新写入的内容。
            ensure_expected_revision(&rechecked.revision, Some(&current.revision))?;
            // 目标在目录同步期间可能被外部修改或替换；只有原封存文件仍在才允许移除源。
            pending_destination.verify_for_commit()?;
            fs::remove_file(&source).map_err(|_| {
                // Translate a failed filesystem stage into the stable vault-storage error category.
                MarkdownWorkspaceError::StorageUnavailable
            })?;
            // source实际unlink是第二个可见集合变化，必须再撤掉中途启动的扫描。
            publication.changed();
            pending_destination.disarm();
            synchronize_directory(source_parent)?;
            if destination_parent != source_parent {
                // A cross-directory rewritten move also requires destination-directory durability after removal.
                synchronize_directory(&destination_parent)?;
            }
            publication.changed();
            Ok(receipt)
        })();
        // 闭包内的pending/inspect guard此时已全部析构，不能提前取未确认标量。
        publication.finish(result)
    }

    /// Create an exclusive revision-checked copy while guarding it until source validation and durability complete.
    pub fn copy_note(
        &self,
        relative_path: &str,
        expected_revision: Option<&str>,
    ) -> Result<MarkdownNoteMutationReceipt, MarkdownWorkspaceError> {
        // 公开入口始终使用真实目录sync；故障注入仅由同模块私有测试调用。
        self.copy_note_with_directory_sync(relative_path, expected_revision, sync_directory)
    }

    /// 同一次结构操作的私有目录持久化边界，不新增公开flag或运行时重试。
    fn copy_note_with_directory_sync(
        &self,
        relative_path: &str,
        expected_revision: Option<&str>,
        mut synchronize_directory: impl FnMut(&Path) -> Result<(), MarkdownWorkspaceError>,
    ) -> Result<MarkdownNoteMutationReceipt, MarkdownWorkspaceError> {
        // 先声明撤权边界，所有后建文件/rollback guard先drop；纯预校验不激活。
        let publication = WorkspaceStructurePublication::new(self);
        let result = (|| {
            let source = resolve_existing_markdown(&self.canonical_root, relative_path)?;
            let parent = source
                .parent()
                .ok_or(MarkdownWorkspaceError::InvalidRelativePath)?;
            let source_title = markdown_title(&source)?;
            let inspected = inspect_bounded_markdown(&source, MAX_DOCUMENT_BYTES, |content| {
                // 同一次冻结值的摘要与流式字节共同约束目标；不创建整篇slice或String副本。
                create_collision_safe_note_copy(&publication, parent, &source_title, content)
            })?;
            ensure_expected_revision(&inspected.revision, expected_revision)?;
            let mut created = inspected.value;
            synchronize_directory(parent)?;
            created.verify_for_commit()?;
            let receipt =
                mutation_receipt(&self.canonical_root, &created.path, inspected.revision)?;
            created.disarm();
            publication.changed();
            Ok(receipt)
        })();
        // 闭包内的pending/inspect guard此时已全部析构，不能提前取未确认标量。
        publication.finish(result)
    }

    /// Delete one confined revision-checked note and invalidate stale search results.
    pub fn delete_note(
        &self,
        relative_path: &str,
        expected_revision: Option<&str>,
    ) -> Result<(), MarkdownWorkspaceError> {
        // 公开入口始终使用真实目录sync；故障注入仅由同模块私有测试调用。
        self.delete_note_with_directory_sync(relative_path, expected_revision, sync_directory)
    }

    /// 同一次结构操作的私有目录持久化边界，不新增公开flag或运行时重试。
    fn delete_note_with_directory_sync(
        &self,
        relative_path: &str,
        expected_revision: Option<&str>,
        mut synchronize_directory: impl FnMut(&Path) -> Result<(), MarkdownWorkspaceError>,
    ) -> Result<(), MarkdownWorkspaceError> {
        // 先声明撤权边界，所有后建文件/rollback guard先drop；纯预校验不激活。
        let publication = WorkspaceStructurePublication::new(self);
        let result = (|| {
            let path = resolve_existing_markdown(&self.canonical_root, relative_path)?;
            let current = inspect_bounded_markdown(&path, MAX_DOCUMENT_BYTES, |_| {
                // 无正文返回值也必须在有限源窗口中完成校验和真实页面回收。
                Ok(())
            })?;
            ensure_expected_revision(&current.revision, expected_revision)?;
            let parent = path
                .parent()
                .ok_or(MarkdownWorkspaceError::InvalidRelativePath)?;
            fs::remove_file(&path).map_err(|_| {
                // Translate a failed filesystem stage into the stable vault-storage error category.
                MarkdownWorkspaceError::StorageUnavailable
            })?;
            // unlink已经成功，后续sync返回Err也不能继续交付旧页。
            publication.changed();
            synchronize_directory(parent)?;
            publication.changed();
            Ok(())
        })();
        // 闭包内的pending/inspect guard此时已全部析构，不能提前取未确认标量。
        publication.finish(result)
    }

    /// Return a confined canonical note path for an explicit platform operation.
    pub fn resolve_note_for_system_action(
        &self,
        relative_path: &str,
    ) -> Result<PathBuf, MarkdownWorkspaceError> {
        resolve_existing_markdown(&self.canonical_root, relative_path)
    }

    /// Preserve the legacy optimistic-save contract, retaining conflicting text in private recovery storage.
    pub fn save_note(
        &self,
        relative_path: &str,
        content: &str,
        expected_revision: &str,
    ) -> Result<MarkdownSaveReceipt, MarkdownWorkspaceError> {
        // 公开生产入口始终执行真实目录持久化；没有全局故障开关或新增公开测试API。
        self.save_note_with_directory_sync(
            relative_path,
            content,
            expected_revision,
            sync_directory,
        )
    }

    /// 单次私有持久化参数沿用原生保存的测试边界，验证真实rename后的失败撤权时点。
    fn save_note_with_directory_sync(
        &self,
        relative_path: &str,
        content: &str,
        expected_revision: &str,
        synchronize_directory: impl FnOnce(&Path) -> Result<(), MarkdownWorkspaceError>,
    ) -> Result<MarkdownSaveReceipt, MarkdownWorkspaceError> {
        if content.len() as u64 > MAX_DOCUMENT_BYTES {
            // Reject oversized content before opening or altering any note or draft.
            return Err(MarkdownWorkspaceError::DocumentTooLarge);
        }
        let path = resolve_existing_markdown(&self.canonical_root, relative_path)?;
        let canonical_relative_path = relative_markdown_path(&self.canonical_root, &path)?;
        let current = inspect_bounded_markdown(&path, MAX_DOCUMENT_BYTES, |_| {
            // 旧显式保存入口也复用有界disk基线读取，不额外把整文件驻留为校验临时值。
            Ok(())
        })?;
        if current.revision != expected_revision {
            // A preexisting external revision is preserved and the unsaved local text becomes a recovery draft.
            write_conflict_draft(
                &self.conflict_recovery_directory,
                &canonical_relative_path,
                content,
                &self.save_sequence,
            )?;
            return Err(MarkdownWorkspaceError::SaveConflict);
        }
        let original_permissions = path
            .metadata()
            .map_err(|_| {
                // Translate a failed filesystem stage into the stable vault-storage error category.
                MarkdownWorkspaceError::StorageUnavailable
            })?
            .permissions();
        let parent = path
            .parent()
            .ok_or(MarkdownWorkspaceError::InvalidRelativePath)?;
        ensure_within_root(&self.canonical_root, parent)?;

        // Stream and sync complete bytes under rollback ownership before making a final revision check.
        let mut temporary = create_temporary_markdown_save(parent, &self.save_sequence)?;
        temporary
            .file
            .set_permissions(original_permissions)
            .map_err(|_| {
                // Translate a failed filesystem stage into the stable vault-storage error category.
                MarkdownWorkspaceError::StorageUnavailable
            })?;
        temporary.file.write_all(content.as_bytes()).map_err(|_| {
            // Translate a failed filesystem stage into the stable vault-storage error category.
            MarkdownWorkspaceError::StorageUnavailable
        })?;
        temporary.file.sync_all().map_err(|_| {
            // Translate a failed filesystem stage into the stable vault-storage error category.
            MarkdownWorkspaceError::StorageUnavailable
        })?;

        let rechecked = inspect_bounded_markdown(&path, MAX_DOCUMENT_BYTES, |_| {
            // 发布临时文件前重新流式核同一磁盘摘要，失败保持原有冲突保护。
            Ok(())
        })?;
        if rechecked.revision != expected_revision {
            // A revision change during streaming similarly preserves the external file and persists the local draft.
            write_conflict_draft(
                &self.conflict_recovery_directory,
                &canonical_relative_path,
                content,
                &self.save_sequence,
            )?;
            return Err(MarkdownWorkspaceError::SaveConflict);
        }
        // Atomic publication transfers the temporary name; then rebind and replace active buffer ownership.
        fs::rename(&temporary.path, &path).map_err(|_| {
            // Translate a failed filesystem stage into the stable vault-storage error category.
            MarkdownWorkspaceError::StorageUnavailable
        })?;
        temporary.disarm();
        // rename已发布同路径正文，先撤旧搜索，即使后续持久化返回Err；目录排序键仍可继续。
        self.invalidate_content_search();
        synchronize_directory(parent)?;

        let metadata = path.metadata().map_err(|_| {
            // Translate a failed filesystem stage into the stable vault-storage error category.
            MarkdownWorkspaceError::StorageUnavailable
        })?;
        Ok(MarkdownSaveReceipt {
            revision: revision_for_bytes(content.as_bytes()),
            byte_count: content.len() as u64,
            modified_at_ms: modified_at_ms(&metadata),
        })
    }
}

#[cfg(feature = "native-markdown-candidate")]
/// Read a stable disk revision while mapping read failures into the native save-conflict contract.
fn inspect_markdown_save_baseline(
    path: &Path,
) -> Result<InspectedMarkdown<()>, MarkdownWorkspaceError> {
    inspect_bounded_markdown(path, MAX_DOCUMENT_BYTES, |_| {
        // 原生保存基线无全文镜像，只有完整有界校验和真实归还后才授予覆盖权限。
        Ok(())
    })
    .map_err(|_| {
        // Unverifiable association or revision state cannot authorize replacing authoritative disk bytes.
        MarkdownWorkspaceError::SaveConflict
    })
}

#[cfg(feature = "native-markdown-candidate")]
/// Adopt identical persisted content without changing canonical text, generation, selection or history.
fn adopt_matching_document_buffer_baseline(
    document: &mut MarkdownDocumentBuffer,
    path: &Path,
    expected_revision: &str,
) -> Result<MarkdownSaveReceipt, MarkdownWorkspaceError> {
    let replacement = MarkdownDocumentBuffer::open(path, MAX_DOCUMENT_BYTES)
        .map_err(markdown_document_buffer_save_error)?;
    let revision = replacement
        .original_revision()
        // 普通保存的校验模型必须有disk基线，旧恢复模型不具备收敛权限。
        .ok_or(MarkdownWorkspaceError::PendingLegacyRecovery)?
        .encode_hex();
    if revision != expected_revision || !replacement.original_file_identity_is_unchanged() {
        // A revision or identity mismatch means another writer changed the proposed reload baseline.
        return Err(MarkdownWorkspaceError::SaveConflict);
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| {
        // Unverifiable association or revision state cannot authorize replacing authoritative disk bytes.
        MarkdownWorkspaceError::SaveConflict
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        // The reloaded path must remain a regular non-symlink note before publication.
        return Err(MarkdownWorkspaceError::SaveConflict);
    }
    let receipt = MarkdownSaveReceipt {
        revision,
        byte_count: replacement.byte_len() as u64,
        modified_at_ms: modified_at_ms(&metadata),
    };
    if Some(
        document
            .current_revision()
            .map_err(markdown_document_buffer_save_error)?,
    ) != replacement.original_revision()
    {
        // A raced different file cannot change live characters through a persistence-only operation.
        return Err(MarkdownWorkspaceError::SaveConflict);
    }
    // Identical bytes include a retry after our own rename/durability failure. Finish directory sync
    // before accepting the new baseline; neither retry nor an identical external save erases history.
    let parent = path
        .parent()
        .ok_or(MarkdownWorkspaceError::InvalidRelativePath)?;
    sync_directory(parent)?;
    document
        .adopt_saved_baseline(&replacement)
        .map_err(markdown_document_buffer_save_error)?;
    Ok(receipt)
}

#[cfg(feature = "native-markdown-candidate")]
/// Translate document-buffer failures into the stable workspace opening API.
fn markdown_document_buffer_open_error(
    error: MarkdownDocumentBufferError,
) -> MarkdownWorkspaceError {
    match error {
        // 未知旧base必须保留具名冲突，不能被一般I/O分类掩盖。
        MarkdownDocumentBufferError::PendingLegacyRecovery => {
            MarkdownWorkspaceError::PendingLegacyRecovery
        }
        // Preserve the public document size limit failure.
        MarkdownDocumentBufferError::DocumentTooLarge
        // 匿名编辑元数据有独立硬预算；直接编辑API保留typed cause，此稳定边界沿现有容量分类。
        | MarkdownDocumentBufferError::ChangeJournalLimitExceeded => MarkdownWorkspaceError::DocumentTooLarge,
        // Preserve invalid-encoding diagnostics instead of silently replacing bytes.
        MarkdownDocumentBufferError::InvalidUtf8Document => {
            MarkdownWorkspaceError::InvalidUtf8Document
        }
        // Opening-time mutation is reported as an unstable read.
        MarkdownDocumentBufferError::DocumentChangedDuringOpen => {
            MarkdownWorkspaceError::DocumentChangedDuringRead
        }
        // A save-time identity failure is a public save conflict.
        MarkdownDocumentBufferError::DocumentChangedDuringSave => {
            MarkdownWorkspaceError::SaveConflict
        }
        // An unavailable or nonregular note cannot be safely opened through the note API.
        MarkdownDocumentBufferError::OpenUnavailable
        | MarkdownDocumentBufferError::NotRegularFile => MarkdownWorkspaceError::NoteNotFound,
        // 坐标、追加或替换输入失败均不能提供可用文档；此打开边界不暴露内部流式命令细节。
        MarkdownDocumentBufferError::RangeOutsideDocument
        | MarkdownDocumentBufferError::NotUnicodeBoundary
        | MarkdownDocumentBufferError::EditBufferLimitExceeded
        | MarkdownDocumentBufferError::WriteUnavailable
        | MarkdownDocumentBufferError::ReplacementInputUnavailable
        | MarkdownDocumentBufferError::ReplacementRangesNotOrdered
        | MarkdownDocumentBufferError::FindInterrupted
        // 输入独占与历史预留失败不授予打开/保存权限，也不应被解释为保存成功。
        | MarkdownDocumentBufferError::TextInputPrepared
        | MarkdownDocumentBufferError::PreparedTextInputStale
        | MarkdownDocumentBufferError::EditHistoryUnavailable => {
            MarkdownWorkspaceError::StorageUnavailable
        }
        // 这里只是未提交的匿名编辑元数据失败，不代表用户笔记发生文件回滚。
        MarkdownDocumentBufferError::ChangeJournalUnavailable
        | MarkdownDocumentBufferError::ChangeJournalCleanupUnconfirmed
        // 原映射读取/归还失败或同时租约耗尽不能交付可用文档；保留存储失败语义。
        | MarkdownDocumentBufferError::OriginalBackingUnavailable(_)
        | MarkdownDocumentBufferError::EditStorageUnavailable(_)
        // 显示来源目录的真实读取/锁失败仍是存储错误，不能让打开或保存领取成功收据。
        | MarkdownDocumentBufferError::DisplayStorageUnavailable(_)
        | MarkdownDocumentBufferError::OriginalReadLeaseLimitExceeded
        | MarkdownDocumentBufferError::Utf16ReadCacheUnavailable
        | MarkdownDocumentBufferError::RevisionCacheUnavailable => MarkdownWorkspaceError::StorageUnavailable,
        // Viewport budget errors are internal to presentation and cannot provide a usable opened note.
        MarkdownDocumentBufferError::MaterializationLimitExceeded => {
            MarkdownWorkspaceError::StorageUnavailable
        }
    }
}

#[cfg(feature = "native-markdown-candidate")]
/// Translate streaming and saved-snapshot validation failures into stable persistence errors.
fn markdown_document_buffer_save_error(
    error: MarkdownDocumentBufferError,
) -> MarkdownWorkspaceError {
    match error {
        // 恢复内容不因相同bytes或一次普通保存调用就获得覆盖当前磁盘的权限。
        MarkdownDocumentBufferError::PendingLegacyRecovery => {
            MarkdownWorkspaceError::PendingLegacyRecovery
        }
        // Preserve size-limit failures for the caller.
        MarkdownDocumentBufferError::DocumentTooLarge
        // 保存边界沿现有容量分类；日志详细原因仍由直接编辑API提供。
        | MarkdownDocumentBufferError::ChangeJournalLimitExceeded => MarkdownWorkspaceError::DocumentTooLarge,
        // Preserve invalid UTF-8 diagnostics during saved-snapshot validation.
        MarkdownDocumentBufferError::InvalidUtf8Document => {
            MarkdownWorkspaceError::InvalidUtf8Document
        }
        // A changed file during open or adoption prevents declaring a successful save baseline.
        MarkdownDocumentBufferError::DocumentChangedDuringOpen
        | MarkdownDocumentBufferError::DocumentChangedDuringSave => {
            MarkdownWorkspaceError::SaveConflict
        }
        // 其他存储、坐标或替换输入错误不能形成成功保存收据，保留同一稳定边界分类。
        MarkdownDocumentBufferError::OpenUnavailable
        // 分块映射失败或并发租约耗尽不能形成保存收据，不能降级为空内容。
        | MarkdownDocumentBufferError::OriginalBackingUnavailable(_)
        | MarkdownDocumentBufferError::EditStorageUnavailable(_)
        // 显示来源目录的真实读取/锁失败仍是存储错误，不能让打开或保存领取成功收据。
        | MarkdownDocumentBufferError::DisplayStorageUnavailable(_)
        | MarkdownDocumentBufferError::OriginalReadLeaseLimitExceeded
        | MarkdownDocumentBufferError::Utf16ReadCacheUnavailable
        | MarkdownDocumentBufferError::RevisionCacheUnavailable
        | MarkdownDocumentBufferError::NotRegularFile
        | MarkdownDocumentBufferError::RangeOutsideDocument
        | MarkdownDocumentBufferError::NotUnicodeBoundary
        | MarkdownDocumentBufferError::EditBufferLimitExceeded
        | MarkdownDocumentBufferError::WriteUnavailable
        | MarkdownDocumentBufferError::ReplacementInputUnavailable
        | MarkdownDocumentBufferError::ReplacementRangesNotOrdered
        | MarkdownDocumentBufferError::FindInterrupted
        // 输入独占与历史预留失败不授予打开/保存权限，也不应被解释为保存成功。
        | MarkdownDocumentBufferError::TextInputPrepared
        | MarkdownDocumentBufferError::PreparedTextInputStale
        | MarkdownDocumentBufferError::EditHistoryUnavailable => {
            MarkdownWorkspaceError::StorageUnavailable
        }
        // 不把匿名日志清理不确定误当用户文件回滚或成功保存。
        MarkdownDocumentBufferError::ChangeJournalUnavailable
        | MarkdownDocumentBufferError::ChangeJournalCleanupUnconfirmed => MarkdownWorkspaceError::StorageUnavailable,
        // Presentation materialization cannot supply a successful save result.
        MarkdownDocumentBufferError::MaterializationLimitExceeded => {
            MarkdownWorkspaceError::StorageUnavailable
        }
    }
}

/// Apply an optional optimistic concurrency precondition before a note mutation.
fn ensure_expected_revision(
    current_revision: &str,
    expected_revision: Option<&str>,
) -> Result<(), MarkdownWorkspaceError> {
    if expected_revision.is_none_or(|expected| {
        // An explicitly supplied optimistic baseline must exactly match the inspected current revision.
        current_revision == expected
    }) {
        // An omitted precondition or exact matching revision permits the requested mutation.
        Ok(())
    } else {
        // A mismatching revision preserves the external note and reports a conflict.
        Err(MarkdownWorkspaceError::SaveConflict)
    }
}

/// Describe the authoritative result of a completed note operation without reading its body again.
fn mutation_receipt(
    canonical_root: &Path,
    path: &Path,
    revision: String,
) -> Result<MarkdownNoteMutationReceipt, MarkdownWorkspaceError> {
    let metadata = path.metadata().map_err(|_| {
        // Translate a failed filesystem stage into the stable vault-storage error category.
        MarkdownWorkspaceError::StorageUnavailable
    })?;
    Ok(MarkdownNoteMutationReceipt {
        relative_path: relative_markdown_path(canonical_root, path)?,
        title: markdown_title(path)?,
        revision,
        byte_count: metadata.len(),
        modified_at_ms: modified_at_ms(&metadata),
    })
}

/// Validate one trimmed filename component against character and control-character limits.
fn validate_entry_name(
    requested: &str,
    max_characters: usize,
    invalid_error: MarkdownWorkspaceError,
) -> Result<&str, MarkdownWorkspaceError> {
    let name = requested.trim();
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.chars().count() > max_characters
        || name.chars().any(|character| {
            // Reject separators, NUL, and control characters that cannot safely represent one user-visible entry name.
            character == '/' || character == '\0' || character.is_control()
        })
    {
        // Empty, traversing, oversized, or control-bearing names cannot safely round-trip through vault operations.
        return Err(invalid_error);
    }
    Ok(name)
}

/// Reject linked, special, or non-UTF-8 external source names before import.
fn validate_external_import_source(path: &Path) -> Result<(), MarkdownWorkspaceError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| {
        // A failed external-source read or lookup must not be reported as a successfully imported file.
        MarkdownWorkspaceError::ImportSourceUnavailable
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        // Only a regular non-symlink external file can be imported.
        return Err(MarkdownWorkspaceError::ImportSourceUnavailable);
    }
    if path
        .file_name()
        .and_then(|name| {
            // Only UTF-8 names can be safely exposed and reproduced by the public path API.
            name.to_str()
        })
        .is_none()
    {
        // An unsupported filename cannot be displayed or safely reproduced in the vault.
        return Err(MarkdownWorkspaceError::UnsupportedFileName);
    }
    Ok(())
}

/// Distinguish unavailable external input from unavailable vault destination storage.
fn markdown_import_read_error(error: MarkdownWorkspaceError) -> MarkdownWorkspaceError {
    match error {
        // Generic mapped-read storage failure refers to the external source at this boundary.
        MarkdownWorkspaceError::StorageUnavailable => {
            MarkdownWorkspaceError::ImportSourceUnavailable
        }
        // Specific size, encoding, or concurrent-change failures retain their original meaning.
        other => other,
    }
}

/// 完整写入并同步的取得者：保留原O_EXCL描述符，后续清理不得从路径重新认领身份。
struct AcquiredMarkdownFile {
    file: File,
    identity: FileIdentity,
    parent: File,
    parent_path: PathBuf,
}

impl AcquiredMarkdownFile {
    /// 只在完整write+sync后封身份；rename可改ctime，另存必须在发布后从原FD封存。
    fn seal(
        file: File,
        acquired: FileIdentity,
        expected_bytes: u64,
        parent: File,
        parent_path: PathBuf,
        canonical_root: &Path,
    ) -> std::io::Result<Self> {
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.len() != expected_bytes
            || metadata.dev() != acquired.device
            || metadata.ino() != acquired.inode
        {
            // 当前FD不再代表取得的单链接完整载荷，不生成可删除权限。
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
        }
        // 复用附件已有父目录身份/当前canonical confinement校验，不扩展新的FD系统。
        verify_attachment_parent(canonical_root, &parent_path, &parent)
            .map_err(std::io::Error::other)?;
        Ok(Self {
            identity: FileIdentity::from(&metadata),
            file,
            parent,
            parent_path,
        })
    }
}

/// 以独占方式创建完整笔记；失败partial无可信终态身份，保留文件并报告待核。
fn create_new_markdown_file(
    publication: &WorkspaceStructurePublication<'_>,
    path: &Path,
    content: &[u8],
) -> std::io::Result<AcquiredMarkdownFile> {
    create_new_markdown_file_with_permissions(publication, path, content, None)
}

/// 可选权限、完整写入和同步成功后才转交持FD的清理权限。
fn create_new_markdown_file_with_permissions(
    publication: &WorkspaceStructurePublication<'_>,
    path: &Path,
    content: &[u8],
    permissions: Option<fs::Permissions>,
) -> std::io::Result<AcquiredMarkdownFile> {
    // 生产固定write_all；私有测试只在真实创建后注入写入故障，不新增公开开关。
    create_new_markdown_file_with_writer(publication, path, content, permissions, |file, bytes| {
        file.write_all(bytes)
    })
}

/// 失败时不猜测partial内容的作者；身份无法封存也必须保留路径和首因。
fn create_new_markdown_file_with_writer(
    publication: &WorkspaceStructurePublication<'_>,
    path: &Path,
    content: &[u8],
    permissions: Option<fs::Permissions>,
    write_content: impl FnOnce(&mut File, &[u8]) -> std::io::Result<()>,
) -> std::io::Result<AcquiredMarkdownFile> {
    // 旧有限新建/故障测试入口与流式源共用同一O_EXCL/封存实现，不复制第二套回滚政策。
    create_new_markdown_file_from_stream(
        publication,
        path,
        content.len() as u64,
        permissions,
        |file| write_content(file, content),
    )
}

/// 接收已声明的完整字节数与一次writer，既有完整写入封存、目录身份和失败保留规则不变。
fn create_new_markdown_file_from_stream(
    publication: &WorkspaceStructurePublication<'_>,
    path: &Path,
    expected_bytes: u64,
    permissions: Option<fs::Permissions>,
    write_content: impl FnOnce(&mut File) -> std::io::Result<()>,
) -> std::io::Result<AcquiredMarkdownFile> {
    let parent_path = path
        .parent()
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?
        .to_owned();
    let parent = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&parent_path)?;
    verify_attachment_parent(publication.canonical_root(), &parent_path, &parent)
        .map_err(std::io::Error::other)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    // 从open开始名字已可见；任何后续错误都要撤掉中途扫描看到的部分记录。
    publication.changed();
    let complete = (|| {
        let acquired = FileIdentity::from(&file.metadata()?);
        if let Some(permissions) = permissions {
            // 权限在写入之前设置，失败仍留下未被授权清理的实际取得文件。
            file.set_permissions(permissions)?;
        }
        write_content(&mut file)?;
        file.sync_all()?;
        AcquiredMarkdownFile::seal(
            file,
            acquired,
            expected_bytes,
            parent,
            parent_path,
            publication.canonical_root(),
        )
    })();
    if complete.is_err() {
        // 失败partial即使仍同inode也不能证明没有外部修改；不reseal、不unlink。
        publication.rollback_unconfirmed();
    }
    complete
}

/// Use the platform's exclusive rename primitive so a destination race cannot overwrite another entry.
fn rename_without_replacement(source: &Path, destination: &Path) -> std::io::Result<()> {
    let source = CString::new(source.as_os_str().as_bytes()).map_err(|_| {
        // Reject interior-NUL paths before passing C strings to the exclusive rename primitive.
        std::io::Error::from(std::io::ErrorKind::InvalidInput)
    })?;
    let destination = CString::new(destination.as_os_str().as_bytes()).map_err(|_| {
        // Reject interior-NUL paths before passing C strings to the exclusive rename primitive.
        std::io::Error::from(std::io::ErrorKind::InvalidInput)
    })?;
    #[cfg(target_os = "macos")]
    // SAFETY: both C strings represent validated live workspace paths; RENAME_EXCL forbids replacement.
    let status =
        unsafe { libc::renamex_np(source.as_ptr(), destination.as_ptr(), libc::RENAME_EXCL) };
    #[cfg(target_os = "linux")]
    // SAFETY: both NUL-terminated paths remain live; AT_FDCWD resolves them normally and
    // RENAME_NOREPLACE preserves any preexisting destination atomically on supported Linux filesystems.
    let status = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let status = {
        // Other Unix targets have no verified exclusive-rename implementation in this contract.
        let _ = (source, destination);
        return Err(std::io::Error::from(std::io::ErrorKind::Unsupported));
    };
    if status == 0 {
        // A zero native status transfers the source entry to its new exclusive name.
        Ok(())
    } else {
        // The native error preserves collision information for public operation-specific handling.
        Err(std::io::Error::last_os_error())
    }
}

/// 新公开笔记的有限回滚权限；只清理取得者未变的完整文件，不按路径盲删。
struct PendingVaultFile<'publication, 'session> {
    publication: &'publication WorkspaceStructurePublication<'session>,
    acquired: AcquiredMarkdownFile,
    path: PathBuf,
    armed: bool,
}

impl<'publication, 'session> PendingVaultFile<'publication, 'session> {
    /// 同时转移原取得FD与封存身份，路径本身不能授予清理权限。
    fn new(
        path: PathBuf,
        acquired: AcquiredMarkdownFile,
        publication: &'publication WorkspaceStructurePublication<'session>,
    ) -> Self {
        Self {
            path,
            acquired,
            armed: true,
            publication,
        }
    }

    /// 成功提交之后只关闭FD，不再撤销用户可见的文件。
    fn disarm(&mut self) {
        self.armed = false;
    }

    /// 原取得FD、父目录与当前路径必须仍指向同一封存值；缺失只对回滚表示无需删除。
    fn verify_current(&self) -> std::io::Result<bool> {
        // 即使file仍同inode，父目录被移出库或以symlink接回也不得在库外清理。
        verify_attachment_parent(
            self.publication.canonical_root(),
            &self.acquired.parent_path,
            &self.acquired.parent,
        )
        .map_err(std::io::Error::other)?;
        let original = self.acquired.file.metadata()?;
        if !original.is_file()
            || original.nlink() != 1
            || FileIdentity::from(&original) != self.acquired.identity
        {
            // 同inode写入、权限/链接改变或原文件被替换都使原清理权限失效。
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
        }
        let current = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&self.path)
        {
            // 已不存在的名字不需要删除，且不尝试追踪到其他路径。
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            // symlink/特殊对象/无法核验都保留并交由上层报告。
            Err(error) => return Err(error),
            Ok(current) => current,
        };
        let observed = current.metadata()?;
        if !observed.is_file()
            || observed.nlink() != 1
            || FileIdentity::from(&observed) != self.acquired.identity
        {
            // 同名replacement不能被原FD的所有权代替，外部字节保持原样。
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
        }
        Ok(true)
    }

    /// 提交不能把缺失、改写或替换的目标作为成功结果；原回滚权限保持武装。
    fn verify_for_commit(&self) -> Result<(), MarkdownWorkspaceError> {
        match self.verify_current() {
            // 同一取得文件与路径身份完整，后续提交才可以撤销本次回滚权限。
            Ok(true) => Ok(()),
            // 目标缺失不会授权删除源；外部对象仍由回滚核验保护。
            Ok(false) | Err(_) => Err(MarkdownWorkspaceError::StorageUnavailable),
        }
    }

    /// 对原FD和当前不跟随链接的路径FD验证同一封存值；不在这里重新seal。
    fn rollback(&self) -> std::io::Result<()> {
        if !self.verify_current()? {
            // 已不存在的目标无需删除；不在其他路径寻找原取得文件。
            return Ok(());
        }
        // 与附件回滚相同，检查至unlink是cooperating-writer边界；不声称跨syscall原子隔离。
        fs::remove_file(&self.path)?;
        self.publication.changed();
        // 实际移除之后持久化原父目录；失败表示清理未完整确认，不复活旧页或重试unlink。
        self.acquired.parent.sync_all()
    }
}

impl Drop for PendingVaultFile<'_, '_> {
    fn drop(&mut self) {
        if self.armed && self.rollback().is_err() {
            // Drop不panic、不覆写主错误；外层在所有guard析构后包装原cause和保留说明。
            self.publication.rollback_unconfirmed();
        }
    }
}

/// Create a bounded, nonoverwriting copy candidate while preserving the established localized copy title.
fn create_collision_safe_note_copy<'publication, 'session>(
    publication: &'publication WorkspaceStructurePublication<'session>,
    parent: &Path,
    source_title: &str,
    content: &BoundedMarkdownText,
) -> Result<PendingVaultFile<'publication, 'session>, MarkdownWorkspaceError> {
    for suffix in 1_u16..=1_000 {
        // Try at most one thousand exclusive candidates so collisions cannot create an unbounded loop.
        let copy_title = if suffix == 1 {
            // The first copy uses the established unsuffixed copy label.
            format!("{source_title} 副本")
        } else {
            // Additional copies append a numeric suffix without changing the source title.
            format!("{source_title} 副本 {suffix}")
        };
        let candidate = parent.join(format!("{copy_title}.md"));
        match create_new_markdown_file_from_stream(
            publication,
            &candidate,
            content.len() as u64,
            None,
            |file| content.copy_to(file).map_err(std::io::Error::other),
        ) {
            // Exclusive creation transfers cleanup responsibility into a pending-file guard.
            Ok(acquired) => return Ok(PendingVaultFile::new(candidate, acquired, publication)),
            // An occupied copy name remains untouched while the next suffix is tried.
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            // Other I/O errors abort copying immediately.
            Err(_) => return Err(MarkdownWorkspaceError::StorageUnavailable),
        }
    }
    Err(MarkdownWorkspaceError::NameCollisionLimitExceeded)
}

/// Create an exclusive imported note candidate with bounded numeric collision suffixes.
#[cfg(test)]
fn create_collision_safe_imported_note<'publication, 'session>(
    publication: &'publication WorkspaceStructurePublication<'session>,
    parent: &Path,
    source_title: &str,
    content: &[u8],
) -> Result<PendingVaultFile<'publication, 'session>, MarkdownWorkspaceError> {
    create_collision_safe_imported_note_with_writer(
        publication,
        parent,
        source_title,
        content.len() as u64,
        |file| file.write_all(content),
    )
}

/// 导入的命名与碰撞策略只有这一处；每次成功取得目标FD后才调用同一个冻结源writer。
fn create_collision_safe_imported_note_with_writer<'publication, 'session>(
    publication: &'publication WorkspaceStructurePublication<'session>,
    parent: &Path,
    source_title: &str,
    expected_bytes: u64,
    mut write_content: impl FnMut(&mut File) -> std::io::Result<()>,
) -> Result<PendingVaultFile<'publication, 'session>, MarkdownWorkspaceError> {
    for suffix in 1_u16..=1_000 {
        // Each bounded attempt owns only its candidate path until exclusive creation succeeds.
        let imported_title = if suffix == 1 {
            // Preserve the external source title for the first candidate.
            source_title.to_owned()
        } else {
            // Use a numeric suffix only after an existing entry occupies the original title.
            format!("{source_title} {suffix}")
        };
        let candidate = parent.join(format!("{imported_title}.md"));
        match create_new_markdown_file_from_stream(
            publication,
            &candidate,
            expected_bytes,
            None,
            &mut write_content,
        ) {
            // A successful candidate is guarded until the import transaction validates and commits.
            Ok(acquired) => return Ok(PendingVaultFile::new(candidate, acquired, publication)),
            // Existing entries are preserved and the next suffix is attempted.
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            // Other creation failures abort the import.
            Err(_) => return Err(MarkdownWorkspaceError::StorageUnavailable),
        }
    }
    Err(MarkdownWorkspaceError::NameCollisionLimitExceeded)
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
/// 稳定的存储与并发失败类别；不确定回滚用Box保留原首因，所以错误可Clone但不再Copy。
pub enum MarkdownWorkspaceError {
    /// 文件未被证明仍属于本次完整写入，保留当前路径；cause保留操作首因而非只报清理错误。
    #[error("{cause}；新建文件的回滚未完整确认，请核对目录中保留或变更的文件")]
    RollbackUnconfirmed {
        #[source]
        cause: Box<MarkdownWorkspaceError>,
    },
    #[error("旧冲突草稿尚未解决，请另存为新笔记或明确重新载入磁盘版本")]
    PendingLegacyRecovery,
    #[error("恢复决定记录无效，旧冲突草稿已保留")]
    RecoveryResolutionCorrupted,
    #[error("无法核验恢复决定，当前草稿与磁盘文件已保留")]
    RecoveryResolutionUnverified,
    #[error("恢复决定已发布，但目录持久化未确认；全部文件已保留")]
    RecoveryResolutionDurabilityUncertain,
    #[error("Markdown 固定目录不可用")]
    RootUnavailable,
    #[error("Omni MD 会话不可用")]
    SessionUnavailable,
    #[error("Omni MD 窗口已经关闭")]
    SessionClosed,
    #[error("Markdown 相对路径无效")]
    InvalidRelativePath,
    #[error("Markdown 路径越过固定根目录")]
    PathEscapesRoot,
    #[error("Markdown 文件不存在")]
    NoteNotFound,
    #[error("Markdown 目录不存在")]
    DirectoryNotFound,
    #[error("只允许操作 .md 文件")]
    NotMarkdown,
    #[error("文件名不能安全地显示或往返保存")]
    UnsupportedFileName,
    #[error("Markdown 文件超过当前编辑大小上限")]
    DocumentTooLarge,
    #[error("Markdown 文件不是有效 UTF-8")]
    InvalidUtf8Document,
    #[error("导入源文件不可用")]
    ImportSourceUnavailable,
    #[error("附件文件名无效")]
    InvalidAttachmentName,
    #[error("附件超过当前导入大小上限")]
    AttachmentTooLarge,
    #[error("读取期间文件发生变化，请重试")]
    DocumentChangedDuringRead,
    #[error("磁盘文件已被修改，未覆盖外部版本")]
    SaveConflict,
    #[error("冲突草稿无法安全持久化，请勿关闭 Omni MD")]
    ConflictRecoveryUnavailable,
    #[error("冲突恢复草稿已损坏，未自动加载")]
    ConflictDraftCorrupted,
    #[error("Markdown 存储暂时不可用")]
    StorageUnavailable,
    #[error("笔记标题无效")]
    InvalidNoteTitle,
    #[error("目录名称无效")]
    InvalidDirectoryName,
    #[error("attachments 是保留目录")]
    ReservedDirectoryName,
    #[error("OMNI 根目录不能重命名或删除")]
    RootDirectoryProtected,
    #[error("同名项目已经存在")]
    EntryAlreadyExists,
    #[error("无法生成不冲突的文件名")]
    NameCollisionLimitExceeded,
    #[error("搜索已被更新的查询取消")]
    SearchSuperseded,
    #[error("搜索分页凭据已失效，请重新搜索")]
    InvalidSearchCursor,
    #[error("笔记目录已变化，请刷新后继续")]
    WorkspaceSuperseded,
    #[error("目录分页凭据已失效，请刷新后继续")]
    InvalidWorkspaceCursor,
}

#[cfg(test)]
mod tests;
