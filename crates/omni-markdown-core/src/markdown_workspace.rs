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
mod native_save;
mod paging;
mod paths;
pub use paging::{MarkdownWorkspacePage, MarkdownWorkspacePager};
mod search;
mod storage;

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
    save_sequence: Arc<AtomicU64>,
}

#[derive(Clone)]
/// Lightweight active-session access; its owner must cancel and join any caller-created workers.
pub struct MarkdownSessionAccess {
    canonical_root: PathBuf,
    conflict_recovery_directory: PathBuf,
    search_generation: Arc<AtomicU64>,
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
            match create_new_markdown_file(&candidate, format!("# {title}\n").as_bytes()) {
                // Exclusive creation succeeded; retain the authoritative path and stop trying names.
                Ok(()) => {
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
        sync_directory(&directory)?;
        self.open_note(&relative_markdown_path(&self.canonical_root, &path)?)
    }

    /// Create one validated user directory and durably publish its vault-relative identifier.
    pub fn create_directory(
        &self,
        relative_parent: &str,
        requested_name: &str,
    ) -> Result<String, MarkdownWorkspaceError> {
        let parent = resolve_existing_directory(&self.canonical_root, relative_parent)?;
        let name = validate_entry_name(
            requested_name,
            MAX_DIRECTORY_NAME_CHARACTERS,
            MarkdownWorkspaceError::InvalidDirectoryName,
        )?;
        if parent == self.canonical_root && name.eq_ignore_ascii_case(ATTACHMENTS_DIRECTORY_NAME) {
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
        sync_directory(&parent)?;
        let canonical_directory = directory.canonicalize().map_err(|_| {
            // Translate a failed filesystem stage into the stable vault-storage error category.
            MarkdownWorkspaceError::StorageUnavailable
        })?;
        ensure_within_root(&self.canonical_root, &canonical_directory)?;
        relative_workspace_path(&self.canonical_root, &canonical_directory)
    }

    /// Rename a user directory without replacing existing entries or moving the vault root.
    pub fn rename_directory(
        &self,
        relative_path: &str,
        requested_name: &str,
    ) -> Result<String, MarkdownWorkspaceError> {
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
        if parent == self.canonical_root && name.eq_ignore_ascii_case(ATTACHMENTS_DIRECTORY_NAME) {
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
        sync_directory(parent)?;
        self.cancel_active_search();
        relative_workspace_path(&self.canonical_root, &destination)
    }

    /// Delete an explicitly selected user subtree while protecting the vault root.
    pub fn delete_directory(&self, relative_path: &str) -> Result<(), MarkdownWorkspaceError> {
        let directory = resolve_existing_directory(&self.canonical_root, relative_path)?;
        if directory == self.canonical_root {
            // The root cannot be recursively removed through the directory deletion API.
            return Err(MarkdownWorkspaceError::RootDirectoryProtected);
        }
        let parent = directory
            .parent()
            .ok_or(MarkdownWorkspaceError::InvalidRelativePath)?;
        ensure_within_root(&self.canonical_root, parent)?;
        fs::remove_dir_all(&directory).map_err(|_| {
            // Translate a failed filesystem stage into the stable vault-storage error category.
            MarkdownWorkspaceError::StorageUnavailable
        })?;
        sync_directory(parent)?;
        self.cancel_active_search();
        Ok(())
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
        let inspected = inspect_mapped_markdown(source_path, MAX_DOCUMENT_BYTES, |content| {
            // Create a guarded exclusive destination while the mapped source remains borrowed; later identity checks can still roll it back.
            create_collision_safe_imported_note(
                &destination_parent,
                source_title,
                content.as_bytes(),
            )
        })
        .map_err(markdown_import_read_error)?;
        let mut imported = inspected.value?;
        sync_directory(&destination_parent)?;
        let receipt = mutation_receipt(&self.canonical_root, &imported.path, inspected.revision)?;
        imported.disarm();
        self.cancel_active_search();
        Ok(receipt)
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
        let source = resolve_existing_markdown(&self.canonical_root, relative_path)?;
        let title = validated_note_title(requested_title)?;
        let parent = source
            .parent()
            .ok_or(MarkdownWorkspaceError::InvalidRelativePath)?;
        ensure_within_root(&self.canonical_root, parent)?;
        let destination = parent.join(format!("{title}.md"));

        let current = inspect_mapped_markdown(&source, MAX_DOCUMENT_BYTES, |_| {
            // This caller needs validated revision or completion metadata only; it deliberately retains no mapped source text.
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
        sync_directory(parent)?;
        self.cancel_active_search();
        mutation_receipt(&self.canonical_root, &destination, current.revision)
    }

    /// Move a revision-checked note and rewrite only its imported attachment destinations when necessary.
    pub fn move_note(
        &self,
        relative_path: &str,
        destination_directory: &str,
        expected_revision: Option<&str>,
    ) -> Result<MarkdownNoteMutationReceipt, MarkdownWorkspaceError> {
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
        let destination_relative_path = relative_markdown_path(&self.canonical_root, &destination)?;
        let current = inspect_mapped_markdown(&source, MAX_DOCUMENT_BYTES, |content| {
            // Rewrite only generated attachment destinations when the new parent changes their relative prefix.
            rewrite_moved_note_attachment_links(
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

        let Some(rewritten_content) = current.value else {
            // Unchanged attachment prefixes permit a direct exclusive rename with no rewritten body.
            rename_without_replacement(&source, &destination).map_err(|error| {
                // Preserve collision classification from an exclusive rename while keeping all other platform failures as storage errors.
                match error.kind() {
                    // An occupied plain-move destination is preserved.
                    std::io::ErrorKind::AlreadyExists => MarkdownWorkspaceError::EntryAlreadyExists,
                    // Other plain-move errors leave the original in place.
                    _ => MarkdownWorkspaceError::StorageUnavailable,
                }
            })?;
            sync_directory(source_parent)?;
            if destination_parent != source_parent {
                // A cross-directory rename changes metadata in both parents, so sync both.
                sync_directory(&destination_parent)?;
            }
            self.cancel_active_search();
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
        create_new_markdown_file_with_permissions(
            &destination,
            rewritten_content.as_bytes(),
            Some(source_permissions),
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
        let mut pending_destination = PendingVaultFile::new(destination.clone());
        sync_directory(&destination_parent)?;
        let receipt = mutation_receipt(
            &self.canonical_root,
            &destination,
            revision_for_bytes(rewritten_content.as_bytes()),
        )?;

        // Recheck the original before removal; destination rollback remains armed until removal succeeds.
        let rechecked = inspect_mapped_markdown(&source, MAX_DOCUMENT_BYTES, |_| {
            // This caller needs validated revision or completion metadata only; it deliberately retains no mapped source text.
        })?;
        ensure_expected_revision(&rechecked.revision, expected_revision)?;
        fs::remove_file(&source).map_err(|_| {
            // Translate a failed filesystem stage into the stable vault-storage error category.
            MarkdownWorkspaceError::StorageUnavailable
        })?;
        pending_destination.disarm();
        sync_directory(source_parent)?;
        if destination_parent != source_parent {
            // A cross-directory rewritten move also requires destination-directory durability after removal.
            sync_directory(&destination_parent)?;
        }
        self.cancel_active_search();
        Ok(receipt)
    }

    /// Create an exclusive revision-checked copy while guarding it until source validation and durability complete.
    pub fn copy_note(
        &self,
        relative_path: &str,
        expected_revision: Option<&str>,
    ) -> Result<MarkdownNoteMutationReceipt, MarkdownWorkspaceError> {
        let source = resolve_existing_markdown(&self.canonical_root, relative_path)?;
        let parent = source
            .parent()
            .ok_or(MarkdownWorkspaceError::InvalidRelativePath)?;
        let source_title = markdown_title(&source)?;
        let inspected = inspect_mapped_markdown(&source, MAX_DOCUMENT_BYTES, |content| {
            // Create a guarded exclusive copy from the same bytes whose revision is checked before committing it.
            create_collision_safe_note_copy(parent, &source_title, content.as_bytes())
        })?;
        ensure_expected_revision(&inspected.revision, expected_revision)?;
        let mut created = inspected.value?;
        sync_directory(parent)?;
        let receipt = mutation_receipt(&self.canonical_root, &created.path, inspected.revision)?;
        created.disarm();
        self.cancel_active_search();
        Ok(receipt)
    }

    /// Delete one confined revision-checked note and invalidate stale search results.
    pub fn delete_note(
        &self,
        relative_path: &str,
        expected_revision: Option<&str>,
    ) -> Result<(), MarkdownWorkspaceError> {
        let path = resolve_existing_markdown(&self.canonical_root, relative_path)?;
        let current = inspect_mapped_markdown(&path, MAX_DOCUMENT_BYTES, |_| {
            // This caller needs validated revision or completion metadata only; it deliberately retains no mapped source text.
        })?;
        ensure_expected_revision(&current.revision, expected_revision)?;
        let parent = path
            .parent()
            .ok_or(MarkdownWorkspaceError::InvalidRelativePath)?;
        fs::remove_file(&path).map_err(|_| {
            // Translate a failed filesystem stage into the stable vault-storage error category.
            MarkdownWorkspaceError::StorageUnavailable
        })?;
        sync_directory(parent)?;
        self.cancel_active_search();
        Ok(())
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
        if content.len() as u64 > MAX_DOCUMENT_BYTES {
            // Reject oversized content before opening or altering any note or draft.
            return Err(MarkdownWorkspaceError::DocumentTooLarge);
        }
        let path = resolve_existing_markdown(&self.canonical_root, relative_path)?;
        let canonical_relative_path = relative_markdown_path(&self.canonical_root, &path)?;
        let current = inspect_mapped_markdown(&path, MAX_DOCUMENT_BYTES, |_| {
            // This caller needs validated revision or completion metadata only; it deliberately retains no mapped source text.
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

        let rechecked = inspect_mapped_markdown(&path, MAX_DOCUMENT_BYTES, |_| {
            // This caller needs validated revision or completion metadata only; it deliberately retains no mapped source text.
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
        sync_directory(parent)?;

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
    inspect_mapped_markdown(path, MAX_DOCUMENT_BYTES, |_| {
        // This caller needs validated revision or completion metadata only; it deliberately retains no mapped source text.
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
    let revision = replacement.original_revision().encode_hex();
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
    if document.current_revision() != replacement.original_revision() {
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
        // Preserve the public document size limit failure.
        MarkdownDocumentBufferError::DocumentTooLarge => MarkdownWorkspaceError::DocumentTooLarge,
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
        // Unexpected coordinate, edit-capacity, or writer failures map to unavailable storage at this boundary.
        MarkdownDocumentBufferError::RangeOutsideDocument
        | MarkdownDocumentBufferError::NotUnicodeBoundary
        | MarkdownDocumentBufferError::EditBufferLimitExceeded
        | MarkdownDocumentBufferError::WriteUnavailable => {
            MarkdownWorkspaceError::StorageUnavailable
        }
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
        // Preserve size-limit failures for the caller.
        MarkdownDocumentBufferError::DocumentTooLarge => MarkdownWorkspaceError::DocumentTooLarge,
        // Preserve invalid UTF-8 diagnostics during saved-snapshot validation.
        MarkdownDocumentBufferError::InvalidUtf8Document => {
            MarkdownWorkspaceError::InvalidUtf8Document
        }
        // A changed file during open or adoption prevents declaring a successful save baseline.
        MarkdownDocumentBufferError::DocumentChangedDuringOpen
        | MarkdownDocumentBufferError::DocumentChangedDuringSave => {
            MarkdownWorkspaceError::SaveConflict
        }
        // Other storage and editor-coordinate failures prevent a usable persistence result.
        MarkdownDocumentBufferError::OpenUnavailable
        | MarkdownDocumentBufferError::NotRegularFile
        | MarkdownDocumentBufferError::RangeOutsideDocument
        | MarkdownDocumentBufferError::NotUnicodeBoundary
        | MarkdownDocumentBufferError::EditBufferLimitExceeded
        | MarkdownDocumentBufferError::WriteUnavailable => {
            MarkdownWorkspaceError::StorageUnavailable
        }
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

/// Create an owner-private note exclusively and remove partial bytes after a failed write.
fn create_new_markdown_file(path: &Path, content: &[u8]) -> std::io::Result<()> {
    create_new_markdown_file_with_permissions(path, content, None)
}

/// Create a nonoverwriting note, optionally preserve permissions, then sync complete bytes.
fn create_new_markdown_file_with_permissions(
    path: &Path,
    content: &[u8],
    permissions: Option<fs::Permissions>,
) -> std::io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let write_result = permissions
        .map_or(Ok(()), |permissions| {
            // Preserve requested source permissions before writing any bytes into the new exclusive destination.
            file.set_permissions(permissions)
        })
        .and_then(|()| {
            // Write the complete bounded content only after the new file's permission stage succeeds.
            file.write_all(content)
        })
        .and_then(|()| {
            // Sync all completed note bytes before relinquishing rollback responsibility.
            file.sync_all()
        });
    drop(file);
    if let Err(error) = write_result {
        // A failed permission, write, or sync stage removes the exclusively created partial file before returning.
        let _ = fs::remove_file(path);
        return Err(error);
    }
    Ok(())
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

/// Rollback guard for an exclusive vault file until its transaction publishes a durable receipt.
struct PendingVaultFile {
    path: PathBuf,
    armed: bool,
}

impl PendingVaultFile {
    /// Arm rollback ownership for a newly created but uncommitted vault path.
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    /// Transfer ownership of a committed vault file so rollback no longer unlinks it.
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PendingVaultFile {
    /// Remove an unpublished guarded vault file without panicking or propagating teardown errors.
    fn drop(&mut self) {
        if self.armed {
            // Only an armed pending file remains owned by rollback.
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Create a bounded, nonoverwriting copy candidate while preserving the established localized copy title.
fn create_collision_safe_note_copy(
    parent: &Path,
    source_title: &str,
    content: &[u8],
) -> Result<PendingVaultFile, MarkdownWorkspaceError> {
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
        match create_new_markdown_file(&candidate, content) {
            // Exclusive creation transfers cleanup responsibility into a pending-file guard.
            Ok(()) => return Ok(PendingVaultFile::new(candidate)),
            // An occupied copy name remains untouched while the next suffix is tried.
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            // Other I/O errors abort copying immediately.
            Err(_) => return Err(MarkdownWorkspaceError::StorageUnavailable),
        }
    }
    Err(MarkdownWorkspaceError::NameCollisionLimitExceeded)
}

/// Create an exclusive imported note candidate with bounded numeric collision suffixes.
fn create_collision_safe_imported_note(
    parent: &Path,
    source_title: &str,
    content: &[u8],
) -> Result<PendingVaultFile, MarkdownWorkspaceError> {
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
        match create_new_markdown_file(&candidate, content) {
            // A successful candidate is guarded until the import transaction validates and commits.
            Ok(()) => return Ok(PendingVaultFile::new(candidate)),
            // Existing entries are preserved and the next suffix is attempted.
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            // Other creation failures abort the import.
            Err(_) => return Err(MarkdownWorkspaceError::StorageUnavailable),
        }
    }
    Err(MarkdownWorkspaceError::NameCollisionLimitExceeded)
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
/// Stable vault, mutation, concurrency, and private-recovery failure categories.
pub enum MarkdownWorkspaceError {
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
}

#[cfg(test)]
mod tests;
