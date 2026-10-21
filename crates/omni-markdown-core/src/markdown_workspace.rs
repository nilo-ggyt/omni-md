//! Explicit vault sessions, confined file mutations, and safe persistence authority.

use std::cmp::Ordering;
use std::ffi::CString;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
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
#[cfg(feature = "native-markdown-candidate")]
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
    active_root: Mutex<Option<PathBuf>>,
    search_generation: Arc<AtomicU64>,
    /// Browsing has separate cancellation authority: collection/order changes and
    /// session closure revoke it, while saving content under the same path preserves it.
    browse_generation: Arc<AtomicU64>,
    /// Native persistence shares one exclusive temporary-name sequence across access clones.
    #[cfg(feature = "native-markdown-candidate")]
    save_sequence: Arc<AtomicU64>,
}

#[derive(Clone)]
/// Lightweight active-session access; its owner must cancel and join any caller-created workers.
pub struct MarkdownSessionAccess {
    canonical_root: PathBuf,
    search_generation: Arc<AtomicU64>,
    /// Access clones share revocation counters, never directory handles or an eager index.
    browse_generation: Arc<AtomicU64>,
    /// Native persistence shares one exclusive temporary-name sequence across access clones.
    #[cfg(feature = "native-markdown-candidate")]
    save_sequence: Arc<AtomicU64>,
}

impl MarkdownWorkspace {
    #[must_use]
    /// Configure the single vault path without opening files, creating storage, or starting work.
    pub fn new(configured_root: PathBuf) -> Self {
        // The native model acquires only confined UTF-8 Markdown files. Session configuration
        // therefore owns no alternate source directory or application-data acquisition path.
        Self {
            configured_root,
            active_root: Mutex::new(None),
            search_generation: Arc::new(AtomicU64::new(0)),
            browse_generation: Arc::new(AtomicU64::new(0)),
            #[cfg(feature = "native-markdown-candidate")]
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
            search_generation: Arc::clone(&self.search_generation),
            browse_generation: Arc::clone(&self.browse_generation),
            #[cfg(feature = "native-markdown-candidate")]
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
        // This one confined file supplies both immutable source bytes and the real disk baseline.
        // Unrelated application-data files are neither inspected nor admitted as substitute text.
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

    /// Create a heading note with bounded collision retries and exclusive file creation.
    pub fn create_note(
        &self,
        relative_directory: &str,
        requested_title: &str,
    ) -> Result<MarkdownNoteMutationReceipt, MarkdownWorkspaceError> {
        // The public operation always performs real directory durability. Only
        // the existing in-module filesystem regression can supply its private failure boundary.
        self.create_note_with_directory_sync(relative_directory, requested_title, sync_directory)
    }

    /// One creation operation retains its actual directory-sync and publication owners.
    fn create_note_with_directory_sync(
        &self,
        relative_directory: &str,
        requested_title: &str,
        mut synchronize_directory: impl FnMut(&Path) -> Result<(), MarkdownWorkspaceError>,
    ) -> Result<MarkdownNoteMutationReceipt, MarkdownWorkspaceError> {
        // Declare publication before file/rollback owners so their cleanup finishes
        // first; validation alone does not mark the workspace collection as changed.
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
            // The new name is already visible. Later sync/inspection failure cannot
            // allow an older search's final result to represent this changed collection.
            publication.changed();
            synchronize_directory(&directory)?;
            // Creation publishes only a compact receipt. Validate the actual file through
            // bounded source reads; opening an editor and owning its text are separate operations.
            let created = inspect_bounded_markdown(&path, MAX_DOCUMENT_BYTES, |_| Ok(()))?;
            mutation_receipt(&self.canonical_root, &path, created.revision)
        })();
        // All inner inspection/rollback guards have finished before the publication
        // owner combines the operation result with any unconfirmed cleanup.
        publication.finish(result)
    }

    /// Create one validated user directory and durably publish its vault-relative identifier.
    pub fn create_directory(
        &self,
        relative_parent: &str,
        requested_name: &str,
    ) -> Result<String, MarkdownWorkspaceError> {
        // Production always performs real directory sync; only the private in-module
        // regression entry can select a failing filesystem boundary.
        self.create_directory_with_directory_sync(relative_parent, requested_name, sync_directory)
    }

    /// Keep directory durability inside this one creation operation, with no public
    /// failure switch or automatic replay of a possibly completed mutation.
    fn create_directory_with_directory_sync(
        &self,
        relative_parent: &str,
        requested_name: &str,
        mut synchronize_directory: impl FnMut(&Path) -> Result<(), MarkdownWorkspaceError>,
    ) -> Result<String, MarkdownWorkspaceError> {
        // The publication owner outlives all acquired files and rollback guards;
        // rejecting a name or parent alone must not invalidate directory pages.
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
            // Creation changed the actual collection. Revoke old traversal authority
            // even if the following directory sync reports uncertain durability.
            publication.changed();
            synchronize_directory(&parent)?;
            let canonical_directory = directory.canonicalize().map_err(|_| {
                // Translate a failed filesystem stage into the stable vault-storage error category.
                MarkdownWorkspaceError::StorageUnavailable
            })?;
            ensure_within_root(&self.canonical_root, &canonical_directory)?;
            relative_workspace_path(&self.canonical_root, &canonical_directory)
        })();
        // Observe cleanup only after inner guards have finished, rather than
        // publishing a preliminary result while rollback can still change the tree.
        publication.finish(result)
    }

    /// Rename a user directory without replacing existing entries or moving the vault root.
    pub fn rename_directory(
        &self,
        relative_path: &str,
        requested_name: &str,
    ) -> Result<String, MarkdownWorkspaceError> {
        // The public rename uses real directory durability; a private regression can
        // exercise failure after the actual rename has already become visible.
        self.rename_directory_with_directory_sync(relative_path, requested_name, sync_directory)
    }

    /// Own one rename and its parent-directory durability without replaying a
    /// completed rename when the later sync fails.
    fn rename_directory_with_directory_sync(
        &self,
        relative_path: &str,
        requested_name: &str,
        mut synchronize_directory: impl FnMut(&Path) -> Result<(), MarkdownWorkspaceError>,
    ) -> Result<String, MarkdownWorkspaceError> {
        // Keep revocation alive through all inner resource cleanup. Preflight errors
        // and an unchanged destination leave existing directory authority intact.
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
            // The entry has actually moved; revoke old pages before attempting
            // fallible parent-directory durability.
            publication.changed();
            synchronize_directory(parent)?;
            publication.changed();
            relative_workspace_path(&self.canonical_root, &destination)
        })();
        // Only after inner resources finish can publication report the final
        // operation result and any cleanup uncertainty.
        publication.finish(result)
    }

    /// Delete an explicitly selected user subtree while protecting the vault root.
    pub fn delete_directory(&self, relative_path: &str) -> Result<(), MarkdownWorkspaceError> {
        // The public delete retains the real directory sync; no runtime failure
        // injection or alternative filesystem implementation is selected here.
        self.delete_directory_with_directory_sync(relative_path, sync_directory)
    }

    /// Retain standard recursive deletion and the durability boundary of this
    /// same operation, including partial-removal failures.
    fn delete_directory_with_directory_sync(
        &self,
        relative_path: &str,
        synchronize_directory: impl FnMut(&Path) -> Result<(), MarkdownWorkspaceError>,
    ) -> Result<(), MarkdownWorkspaceError> {
        self.delete_directory_with_removal(relative_path, synchronize_directory, |path| {
            // Use the standard library's actual recursive removal in production.
            fs::remove_dir_all(path)
        })
    }

    /// The private removal boundary lets the existing regression fail after real
    /// partial deletion; production still uses the standard recursive operation.
    fn delete_directory_with_removal(
        &self,
        relative_path: &str,
        mut synchronize_directory: impl FnMut(&Path) -> Result<(), MarkdownWorkspaceError>,
        remove_directory: impl FnOnce(&Path) -> std::io::Result<()>,
    ) -> Result<(), MarkdownWorkspaceError> {
        // Keep publication ownership outside the deletion scope so root protection
        // remains a pure preflight failure and cleanup completes before reporting.
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
            // Recursive removal may delete children before returning an error.
            // After root protection succeeds, conservatively revoke the old scan
            // before invoking that operation rather than assuming failure means no change.
            publication.recursive_delete_attempt();
            remove_directory(&directory).map_err(|_| {
                // Translate a failed filesystem stage into the stable vault-storage error category.
                MarkdownWorkspaceError::StorageUnavailable
            })?;
            synchronize_directory(parent)?;
            publication.changed();
            Ok(())
        })();
        // Inner cleanup is complete before final revocation/error publication.
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
        // The public import always syncs the real destination directory; only the
        // private regression entry can fail that boundary after destination creation.
        self.import_note_with_directory_sync(source_path, destination_directory, sync_directory)
    }

    /// Keep source validation, destination rollback, and directory durability in
    /// one import attempt without replaying external mutations.
    fn import_note_with_directory_sync(
        &self,
        source_path: &Path,
        destination_directory: &str,
        mut synchronize_directory: impl FnMut(&Path) -> Result<(), MarkdownWorkspaceError>,
    ) -> Result<MarkdownNoteMutationReceipt, MarkdownWorkspaceError> {
        // Publication outlives the imported-file guard so failed source validation
        // cannot report completion before destination rollback has actually run.
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
                // Stream only this validated frozen source. A later close or path
                // recheck failure leaves the complete destination under rollback.
                // Nest the destination result so the outer mapping classifies
                // source-read failures without relabeling destination-write failures.
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
        // Source and destination guards have now finished; only this final owner
        // can report the operation together with any retained-file uncertainty.
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
        // Use real parent-directory durability for the public operation; only
        // the private regression can fail sync after the actual rename.
        self.rename_note_with_directory_sync(
            relative_path,
            requested_title,
            expected_revision,
            sync_directory,
        )
    }

    /// Keep revision validation, exclusive rename, and directory sync within one
    /// attempt; a sync error does not justify blindly replaying the rename.
    fn rename_note_with_directory_sync(
        &self,
        relative_path: &str,
        requested_title: &str,
        expected_revision: Option<&str>,
        mut synchronize_directory: impl FnMut(&Path) -> Result<(), MarkdownWorkspaceError>,
    ) -> Result<MarkdownNoteMutationReceipt, MarkdownWorkspaceError> {
        // The publication owner survives all inner resources. Pure name/revision
        // rejection must not claim that the filesystem collection changed.
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
                // The revision still requires finite UTF-8/SHA leases; a callback
                // that needs no text does not authorize touching a whole mapping.
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
            // The name has moved before sync can fail, so revoke pages that still
            // describe the old collection immediately.
            publication.changed();
            synchronize_directory(parent)?;
            publication.changed();
            mutation_receipt(&self.canonical_root, &destination, current.revision)
        })();
        // Complete inner cleanup before combining mutation and publication outcomes.
        publication.finish(result)
    }

    /// Move a revision-checked note and rewrite only its imported attachment destinations when necessary.
    pub fn move_note(
        &self,
        relative_path: &str,
        destination_directory: &str,
        expected_revision: Option<&str>,
    ) -> Result<MarkdownNoteMutationReceipt, MarkdownWorkspaceError> {
        // Production syncs the real source and destination parents; the existing
        // private regression can expose failure between those actual filesystem stages.
        self.move_note_with_directory_sync(
            relative_path,
            destination_directory,
            expected_revision,
            sync_directory,
        )
    }

    /// Preserve one move's source revision, destination ownership, and durability
    /// boundaries without a second implementation or automatic mutation replay.
    fn move_note_with_directory_sync(
        &self,
        relative_path: &str,
        destination_directory: &str,
        expected_revision: Option<&str>,
        mut synchronize_directory: impl FnMut(&Path) -> Result<(), MarkdownWorkspaceError>,
    ) -> Result<MarkdownNoteMutationReceipt, MarkdownWorkspaceError> {
        // Retain publication until every inner source/spool/destination guard has
        // ended; preflight alone cannot invalidate an otherwise unchanged collection.
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
                // Stream only the attachment-prefix rewrite into an anonymous spool.
                // Neither an overlong line nor a URL becomes a whole-document String.
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
                // The direct rename already changed paths. Revoke both search and
                // directory authority before either parent's fallible sync.
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
                    // This attempt exclusively owns the frozen-source spool at offset
                    // zero. Stream/kernel-copy it into the destination without exposing
                    // a whole-body allocation to the application.
                    let copied = std::io::copy(&mut rewritten.file, file)?;
                    if copied != rewritten.byte_len {
                        // A short transfer cannot seal a complete destination or permit
                        // later deletion of the revision-checked original.
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
                // Recompute the complete original revision before removal through
                // finite leases with actual page reclamation.
                Ok(())
            })?;
            ensure_expected_revision(&rechecked.revision, expected_revision)?;
            // Even without a caller precondition, compare with this rewrite's captured
            // revision so bytes written during destination sync are never deleted.
            ensure_expected_revision(&rechecked.revision, Some(&current.revision))?;
            // Directory sync can interleave with external writes/replacement. Only
            // the same sealed destination permits removal of the original source.
            pending_destination.verify_for_commit()?;
            fs::remove_file(&source).map_err(|_| {
                // Translate a failed filesystem stage into the stable vault-storage error category.
                MarkdownWorkspaceError::StorageUnavailable
            })?;
            // Source unlink is a second visible collection change; revoke scans
            // that could have started after destination creation but before removal.
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
        // Observe rollback uncertainty after all inner guards have actually ended.
        publication.finish(result)
    }

    /// Create an exclusive revision-checked copy while guarding it until source validation and durability complete.
    pub fn copy_note(
        &self,
        relative_path: &str,
        expected_revision: Option<&str>,
    ) -> Result<MarkdownNoteMutationReceipt, MarkdownWorkspaceError> {
        // The public copy always syncs its real parent; only the private regression
        // entry can fail durability after creating an actual guarded destination.
        self.copy_note_with_directory_sync(relative_path, expected_revision, sync_directory)
    }

    /// Keep one copy's frozen source, revision check, guarded destination, and
    /// directory durability within the same operation owner.
    fn copy_note_with_directory_sync(
        &self,
        relative_path: &str,
        expected_revision: Option<&str>,
        mut synchronize_directory: impl FnMut(&Path) -> Result<(), MarkdownWorkspaceError>,
    ) -> Result<MarkdownNoteMutationReceipt, MarkdownWorkspaceError> {
        // Publication outlives the copy guard so destination rollback finishes
        // before any final operation or cleanup result is exposed.
        let publication = WorkspaceStructurePublication::new(self);
        let result = (|| {
            let source = resolve_existing_markdown(&self.canonical_root, relative_path)?;
            let parent = source
                .parent()
                .ok_or(MarkdownWorkspaceError::InvalidRelativePath)?;
            let source_title = markdown_title(&source)?;
            let inspected = inspect_bounded_markdown(&source, MAX_DOCUMENT_BYTES, |content| {
                // The digest and copied windows belong to the same frozen value;
                // no whole-source slice or String becomes a second authority.
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
        // All source and destination cleanup has completed before final publication.
        publication.finish(result)
    }

    /// Delete one confined revision-checked note and invalidate stale search results.
    pub fn delete_note(
        &self,
        relative_path: &str,
        expected_revision: Option<&str>,
    ) -> Result<(), MarkdownWorkspaceError> {
        // Production retains real parent-directory sync; a private regression can
        // fail only after the actual note removal to exercise truthful invalidation.
        self.delete_note_with_directory_sync(relative_path, expected_revision, sync_directory)
    }

    /// Bind deletion to its inspected revision and real directory durability;
    /// failing sync cannot turn an already completed unlink into an unchanged tree.
    fn delete_note_with_directory_sync(
        &self,
        relative_path: &str,
        expected_revision: Option<&str>,
        mut synchronize_directory: impl FnMut(&Path) -> Result<(), MarkdownWorkspaceError>,
    ) -> Result<(), MarkdownWorkspaceError> {
        // Own revocation outside the deletion scope. A failed precondition is pure;
        // after unlink, stale pages remain revoked even if later durability fails.
        let publication = WorkspaceStructurePublication::new(self);
        let result = (|| {
            let path = resolve_existing_markdown(&self.canonical_root, relative_path)?;
            let current = inspect_bounded_markdown(&path, MAX_DOCUMENT_BYTES, |_| {
                // A body-free result still owes complete validation and actual
                // reclamation through finite source windows before deletion.
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
            // Unlink already succeeded; a later sync error cannot authorize
            // continued delivery of pages describing the deleted note.
            publication.changed();
            synchronize_directory(parent)?;
            publication.changed();
            Ok(())
        })();
        // Finish inner cleanup before publishing the final invalidation outcome.
        publication.finish(result)
    }

    /// Return a confined canonical note path for an explicit platform operation.
    pub fn resolve_note_for_system_action(
        &self,
        relative_path: &str,
    ) -> Result<PathBuf, MarkdownWorkspaceError> {
        resolve_existing_markdown(&self.canonical_root, relative_path)
    }
}

#[cfg(feature = "native-markdown-candidate")]
/// Read a stable disk revision while mapping read failures into the native save-conflict contract.
fn inspect_markdown_save_baseline(
    path: &Path,
) -> Result<InspectedMarkdown<()>, MarkdownWorkspaceError> {
    inspect_bounded_markdown(path, MAX_DOCUMENT_BYTES, |_| {
        // A native save baseline owns no text mirror. Only complete finite-window
        // validation and real reclamation can authorize replacement of disk bytes.
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
    if document
        .current_revision()
        .map_err(markdown_document_buffer_save_error)?
        != replacement.original_revision()
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
        // Preserve the public document size limit failure.
        MarkdownDocumentBufferError::DocumentTooLarge
        // Anonymous edit metadata has its own hard budget. Direct editing keeps
        // the specific typed cause; this public opening boundary uses its size category.
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
        // Coordinate, append, and replacement-input failures cannot yield an opened
        // document; this boundary does not expose internal streaming-command details.
        MarkdownDocumentBufferError::RangeOutsideDocument
        | MarkdownDocumentBufferError::NotUnicodeBoundary
        | MarkdownDocumentBufferError::EditBufferLimitExceeded
        | MarkdownDocumentBufferError::WriteUnavailable
        | MarkdownDocumentBufferError::ReplacementInputUnavailable
        | MarkdownDocumentBufferError::ReplacementRangesNotOrdered
        | MarkdownDocumentBufferError::FindInterrupted
        // Exclusive input and history reservation failures grant neither opening
        // nor persistence authority and must not be interpreted as a successful save.
        | MarkdownDocumentBufferError::TextInputPrepared
        | MarkdownDocumentBufferError::PreparedTextInputStale
        | MarkdownDocumentBufferError::EditHistoryUnavailable => {
            MarkdownWorkspaceError::StorageUnavailable
        }
        // Failed uncommitted anonymous metadata does not mean a user note was rolled back.
        MarkdownDocumentBufferError::ChangeJournalUnavailable
        | MarkdownDocumentBufferError::ChangeJournalCleanupUnconfirmed
        // A source read/reclamation failure or exhausted concurrent lease budget
        // cannot yield a usable document; keep the public storage-failure category.
        | MarkdownDocumentBufferError::OriginalBackingUnavailable(_)
        | MarkdownDocumentBufferError::EditStorageUnavailable(_)
        // Actual display-source read/lock failures remain storage errors and
        // cannot provide either an opened document or a successful save receipt.
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
        // Preserve size-limit failures for the caller.
        MarkdownDocumentBufferError::DocumentTooLarge
        // Preserve the public capacity category; direct editing retains the
        // specific anonymous journal-budget cause for its own caller.
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
        // Storage, coordinate, and replacement-input failures cannot establish
        // a successful save receipt; retain the same stable boundary category.
        MarkdownDocumentBufferError::OpenUnavailable
        // Mapping failure or concurrent lease exhaustion cannot produce a save
        // receipt and must never degrade into an apparently empty document.
        | MarkdownDocumentBufferError::OriginalBackingUnavailable(_)
        | MarkdownDocumentBufferError::EditStorageUnavailable(_)
        // Display-source I/O and lock failures remain real storage failures,
        // never successful persistence of an incomplete display projection.
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
        // Exclusive input and history reservation failures grant no persistence
        // authority even if an earlier stage completed successfully.
        | MarkdownDocumentBufferError::TextInputPrepared
        | MarkdownDocumentBufferError::PreparedTextInputStale
        | MarkdownDocumentBufferError::EditHistoryUnavailable => {
            MarkdownWorkspaceError::StorageUnavailable
        }
        // Unconfirmed anonymous journal cleanup is neither user-file rollback
        // nor evidence of a completed save.
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

/// Own a completely written, synced file through its original exclusive descriptor.
/// A later pathname lookup cannot replace that descriptor's cleanup authority.
struct AcquiredMarkdownFile {
    file: File,
    identity: FileIdentity,
    parent: File,
    parent_path: PathBuf,
}

impl AcquiredMarkdownFile {
    /// Seal identity only after complete write and sync. Rename may change ctime,
    /// so a save-as owner must seal its post-publication identity from the original FD.
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
            // The descriptor no longer proves the acquired single-link complete
            // payload; do not issue authority that could later delete unrelated bytes.
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
        }
        // Reuse the attachment boundary's real parent-descriptor and current
        // canonical-containment checks rather than creating another FD owner graph.
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

/// Exclusively create a complete note. A failed partial write has no sealed
/// terminal identity, so retain the file and report unconfirmed rollback.
fn create_new_markdown_file(
    publication: &WorkspaceStructurePublication<'_>,
    path: &Path,
    content: &[u8],
) -> std::io::Result<AcquiredMarkdownFile> {
    create_new_markdown_file_with_permissions(publication, path, content, None)
}

/// Transfer descriptor-backed cleanup authority only after permissions, complete
/// content writing, and file durability have succeeded.
fn create_new_markdown_file_with_permissions(
    publication: &WorkspaceStructurePublication<'_>,
    path: &Path,
    content: &[u8],
    permissions: Option<fs::Permissions>,
) -> std::io::Result<AcquiredMarkdownFile> {
    // Production writes the complete finite input. The existing private regression
    // can fail after real creation without adding a public failure switch.
    create_new_markdown_file_with_writer(publication, path, content, permissions, |file, bytes| {
        // A successful callback must account for every declared byte before sealing.
        file.write_all(bytes)
    })
}

/// Do not infer ownership of partial bytes after failure. If sealing is impossible,
/// retain the path and original cause rather than guessing that unlink is safe.
fn create_new_markdown_file_with_writer(
    publication: &WorkspaceStructurePublication<'_>,
    path: &Path,
    content: &[u8],
    permissions: Option<fs::Permissions>,
    write_content: impl FnOnce(&mut File, &[u8]) -> std::io::Result<()>,
) -> std::io::Result<AcquiredMarkdownFile> {
    // Finite creation and streaming input share this one exclusive-create/seal
    // implementation and the same policy for unconfirmed partial files.
    create_new_markdown_file_from_stream(
        publication,
        path,
        content.len() as u64,
        permissions,
        |file| write_content(file, content),
    )
}

/// Accept one declared extent and writer while preserving complete-write sealing,
/// parent identity, and retention of files whose cleanup cannot be authorized.
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
    // Retain the original nonlinked directory descriptor as well as its confined
    // pathname before acquiring the new visible file.
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
    // The name is visible as soon as open succeeds. Any later error must revoke
    // scans that could have observed its partial metadata.
    publication.changed();
    let complete = (|| {
        let acquired = FileIdentity::from(&file.metadata()?);
        if let Some(permissions) = permissions {
            // Set requested permissions before writing. Failure still leaves an
            // actual acquired file without sealed authority for automatic deletion.
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
        // A matching inode cannot prove that partial bytes stayed unmodified by
        // another writer; neither reseal nor unlink after this failure.
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

/// Retain finite rollback authority for a newly visible note: only the acquired,
/// unchanged complete file may be removed, never an arbitrary current pathname.
struct PendingVaultFile<'publication, 'session> {
    publication: &'publication WorkspaceStructurePublication<'session>,
    acquired: AcquiredMarkdownFile,
    path: PathBuf,
    armed: bool,
}

impl<'publication, 'session> PendingVaultFile<'publication, 'session> {
    /// Transfer both the original descriptor and sealed identity; the path alone
    /// cannot authorize cleanup of a subsequently replaced entry.
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

    /// After successful commit, release descriptors normally without removing
    /// the now-authoritative user-visible file.
    fn disarm(&mut self) {
        self.armed = false;
    }

    /// Require the acquired FD, original parent, and current name to represent
    /// the same sealed value. Absence means only that rollback has nothing to unlink.
    fn verify_current(&self) -> std::io::Result<bool> {
        // Even the same file inode cannot authorize cleanup after its parent
        // moved outside the vault or was replaced by a symlink back into it.
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
            // In-place writes, permission/link changes, and replacement all revoke
            // the original sealed cleanup authority.
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
        }
        let current = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&self.path)
        {
            // A missing name needs no deletion; never search other paths for the inode.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            // Preserve unsafe or unverifiable names and let the operation owner report them.
            Err(error) => return Err(error),
            // Only a successfully opened, nonfollowed path can be compared with the seal.
            Ok(current) => current,
        };
        let observed = current.metadata()?;
        if !observed.is_file()
            || observed.nlink() != 1
            || FileIdentity::from(&observed) != self.acquired.identity
        {
            // Owning the old FD grants no authority over a replacement under the
            // same name; leave that external value untouched.
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
        }
        Ok(true)
    }

    /// Do not commit a missing, changed, or replaced destination. Keep rollback
    /// armed until the original acquired file and path are both verified.
    fn verify_for_commit(&self) -> Result<(), MarkdownWorkspaceError> {
        match self.verify_current() {
            // Only the intact acquired file/path pair permits later disarming.
            Ok(true) => Ok(()),
            // A missing destination cannot authorize source removal; rollback's
            // identity check continues protecting any external replacement.
            Ok(false) | Err(_) => Err(MarkdownWorkspaceError::StorageUnavailable),
        }
    }

    /// Recheck the original FD and a nonfollowing path FD against the existing
    /// seal; this cleanup path cannot manufacture a new seal for changed bytes.
    fn rollback(&self) -> std::io::Result<()> {
        if !self.verify_current()? {
            // Already-absent output needs no unlink; do not seek the acquired inode elsewhere.
            return Ok(());
        }
        // As in attachment rollback, check-to-unlink assumes cooperating writers;
        // separate syscalls do not provide atomic isolation from a hostile replacement.
        fs::remove_file(&self.path)?;
        self.publication.changed();
        // Sync the retained original parent after actual removal. Failure leaves
        // cleanup unconfirmed; it cannot revive old pages or justify replaying unlink.
        self.acquired.parent.sync_all()
    }
}

impl Drop for PendingVaultFile<'_, '_> {
    /// Attempt only still-owned rollback; report uncertainty through publication
    /// without panicking or replacing an operation's earlier failure.
    fn drop(&mut self) {
        if self.armed && self.rollback().is_err() {
            // The outer owner combines the original cause with retained-file
            // uncertainty only after every inner guard has finished.
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

/// Own the import naming/collision policy in one place. Invoke the same frozen
/// source writer only after exclusively acquiring that attempt's destination FD.
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
/// Stable storage and concurrency outcomes. Boxed compound failures preserve
/// their original cause and additional cleanup facts, so errors are Clone, not Copy.
pub enum MarkdownWorkspaceError {
    /// A frozen-source inspection and its explicit reclamation both failed.
    /// Retain cleanup as a separate typed fact while the source chain starts at
    /// the original consumer/UTF-8-validation failure, never the later close error.
    #[error("{cause}；冻结文档资源归还失败：{cleanup}")]
    InspectionCleanupFailed {
        #[source]
        cause: Box<MarkdownWorkspaceError>,
        cleanup: Box<MarkdownWorkspaceError>,
    },
    /// Retain an entry whose complete-write ownership cannot be re-established.
    /// Preserve the operation's original cause rather than replacing it with cleanup failure.
    #[error("{cause}；新建文件的回滚未完整确认，请核对目录中保留或变更的文件")]
    RollbackUnconfirmed {
        #[source]
        cause: Box<MarkdownWorkspaceError>,
    },
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
