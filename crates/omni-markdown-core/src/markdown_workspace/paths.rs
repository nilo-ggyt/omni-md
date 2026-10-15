//! Canonical vault confinement, bounded traversal, and entry-name validation.

use super::*;

/// Accept a real directory or create it before establishing the canonical vault boundary.
pub(super) fn ensure_workspace_root(path: &Path) -> Result<(), MarkdownWorkspaceError> {
    // Existing roots must remain directories rather than redirects to another filesystem tree.
    if let Ok(metadata) = fs::symlink_metadata(path) {
        // A link or non-directory cannot be the stable root used by all later confinement checks.
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(MarkdownWorkspaceError::RootUnavailable);
        }
        // Reuse the caller-owned directory without changing its permissions or contents.
        return Ok(());
    }
    // A missing root is created on explicit session activation; failures prevent session publication.
    fs::create_dir_all(path).map_err(|_| {
        // An unavailable root cannot become the stable canonical session boundary.
        MarkdownWorkspaceError::RootUnavailable
    })
}

/// Traverse the vault without an entry budget while retaining callback cancellation.
pub(super) fn walk_markdown_workspace(
    canonical_root: &Path,
    visit: impl FnMut(MarkdownFilesystemEntry<'_>) -> Result<WalkControl, MarkdownWorkspaceError>,
) -> Result<(), MarkdownWorkspaceError> {
    walk_markdown_workspace_bounded(canonical_root, usize::MAX, visit).map(|_| {
        // The unbudgeted wrapper exposes completion only; the bounded traversal entry count remains an internal measurement.
    })
}

/// Count every directory entry against a caller budget, skipping links and the attachment tree.
pub(super) fn walk_markdown_workspace_bounded(
    canonical_root: &Path,
    scanned_entry_limit: usize,
    mut visit: impl FnMut(MarkdownFilesystemEntry<'_>) -> Result<WalkControl, MarkdownWorkspaceError>,
) -> Result<usize, MarkdownWorkspaceError> {
    // A zero budget performs no I/O and never invokes the caller callback.
    if scanned_entry_limit == 0 {
        return Ok(0);
    }

    // Keep traversal state local to this call so the inactive session retains no directory handles.
    let mut directories = Vec::with_capacity(scanned_entry_limit.min(32));
    directories.push(canonical_root.to_path_buf());
    let mut scanned_entries = 0_usize;
    // Each directory is opened only while its entries are consumed; queued paths own no handles.
    while let Some(directory) = directories.pop() {
        let mut entries = fs::read_dir(&directory).map_err(|_| {
            // Translate a failed filesystem stage into the stable vault-storage error category.
            MarkdownWorkspaceError::StorageUnavailable
        })?;
        // Advance one entry at a time so cancellation and the budget bound unrelated files as well.
        loop {
            // Stop before requesting another entry once the caller's filesystem budget is exhausted.
            if scanned_entries >= scanned_entry_limit {
                return Ok(scanned_entries);
            }
            let Some(entry) = entries.next() else {
                // Exhausting this directory releases its iterator before visiting the next queued path.
                break;
            };
            // Charge every visited entry before filtering so unrelated file types cannot bypass the scan budget.
            scanned_entries += 1;
            let entry = entry.map_err(|_| {
                // Translate a failed filesystem stage into the stable vault-storage error category.
                MarkdownWorkspaceError::StorageUnavailable
            })?;
            let file_type = entry.file_type().map_err(|_| {
                // Translate a failed filesystem stage into the stable vault-storage error category.
                MarkdownWorkspaceError::StorageUnavailable
            })?;
            // Never descend through, or expose as notes, symbolic links found during traversal.
            if file_type.is_symlink() {
                continue;
            }
            let path = entry.path();
            // Directories are visible to listings and may contain notes unless explicitly reserved.
            if file_type.is_dir() {
                // Imported attachments are data assets, not user note directories or searchable notes.
                if directory == canonical_root
                    && entry.file_name().to_str().is_some_and(|name| {
                        // Reserve the asset namespace consistently even on case-sensitive filesystems.
                        name.eq_ignore_ascii_case(ATTACHMENTS_DIRECTORY_NAME)
                    })
                {
                    continue;
                }
                // Callback stop terminates before queuing this subtree; callback errors propagate unchanged.
                if visit(MarkdownFilesystemEntry::Directory(&path))? == WalkControl::Stop {
                    return Ok(scanned_entries);
                }
                // Retain just the owned path after the callback's borrowed entry expires.
                directories.push(path);
            } else if file_type.is_file()
                && has_markdown_extension(&path)
                && visit(MarkdownFilesystemEntry::Note(&path))? == WalkControl::Stop
            {
                // Only regular Markdown files reach this branch; the callback has requested termination.
                return Ok(scanned_entries);
            }
        }
    }
    Ok(scanned_entries)
}

/// A borrowed entry valid only for the synchronous traversal callback.
pub(super) enum MarkdownFilesystemEntry<'a> {
    Directory(&'a Path),
    Note(&'a Path),
}

/// Callbacks can end traversal without turning intentional cancellation into an error.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum WalkControl {
    Continue,
    Stop,
}

/// Resolve a note through lexical, link, type, and canonical-boundary checks.
pub(super) fn resolve_existing_markdown(
    canonical_root: &Path,
    relative_path: &str,
) -> Result<PathBuf, MarkdownWorkspaceError> {
    // Reject traversal syntax before any filesystem lookup can follow it.
    let relative = validate_relative_path(relative_path, false)?;
    // Files inside the reserved asset tree are never notes even if their suffix is Markdown.
    if targets_reserved_attachments_tree(relative) {
        return Err(MarkdownWorkspaceError::NoteNotFound);
    }
    // Reject redirects in every component rather than only inspecting the final filename.
    if markdown_path_contains_symlink(canonical_root, relative) {
        return Err(MarkdownWorkspaceError::NoteNotFound);
    }
    // The note contract accepts only case-insensitive .md files.
    if !has_markdown_extension(relative) {
        return Err(MarkdownWorkspaceError::NotMarkdown);
    }
    let candidate = canonical_root.join(relative);
    let link_metadata = fs::symlink_metadata(&candidate).map_err(|_| {
        // A missing or unreadable candidate cannot be exposed as a safely resolved regular note.
        MarkdownWorkspaceError::NoteNotFound
    })?;
    // The final entry must still be a regular file when independently inspected.
    if link_metadata.file_type().is_symlink() || !link_metadata.is_file() {
        return Err(MarkdownWorkspaceError::NoteNotFound);
    }
    // Canonicalize and recheck containment; lexical validity alone is not a filesystem boundary.
    let canonical = candidate.canonicalize().map_err(|_| {
        // A missing or unreadable candidate cannot be exposed as a safely resolved regular note.
        MarkdownWorkspaceError::NoteNotFound
    })?;
    ensure_within_root(canonical_root, &canonical)?;
    Ok(canonical)
}

/// Resolve an existing user directory, with the empty relative path denoting the vault root.
pub(super) fn resolve_existing_directory(
    canonical_root: &Path,
    relative_path: &str,
) -> Result<PathBuf, MarkdownWorkspaceError> {
    // Root selection is represented by an empty path and needs no child lookup.
    if relative_path.trim().is_empty() {
        return Ok(canonical_root.to_path_buf());
    }
    let relative = validate_relative_path(relative_path, true)?;
    // Asset storage cannot become a note destination through a directory selection.
    if targets_reserved_attachments_tree(relative) {
        return Err(MarkdownWorkspaceError::DirectoryNotFound);
    }
    // A linked ancestor or destination would make later writes escape the selected tree.
    if markdown_path_contains_symlink(canonical_root, relative) {
        return Err(MarkdownWorkspaceError::DirectoryNotFound);
    }
    let candidate = canonical_root.join(relative);
    let link_metadata = fs::symlink_metadata(&candidate).map_err(|_| {
        // A missing or unreadable candidate cannot be used as a resolved note parent.
        MarkdownWorkspaceError::DirectoryNotFound
    })?;
    // An existing non-directory or link is not a valid parent for note creation.
    if link_metadata.file_type().is_symlink() || !link_metadata.is_dir() {
        return Err(MarkdownWorkspaceError::DirectoryNotFound);
    }
    // Canonicalize and recheck containment; lexical validity alone is not a filesystem boundary.
    let canonical = candidate.canonicalize().map_err(|_| {
        // A missing or unreadable candidate cannot be used as a resolved note parent.
        MarkdownWorkspaceError::DirectoryNotFound
    })?;
    ensure_within_root(canonical_root, &canonical)?;
    Ok(canonical)
}

/// Accept only normal relative components; root selection is handled by the directory resolver.
pub(super) fn validate_relative_path(
    value: &str,
    allow_directory: bool,
) -> Result<&Path, MarkdownWorkspaceError> {
    let path = Path::new(value);
    // Absolute paths and empty note names bypass the caller-selected vault and are rejected.
    if path.is_absolute() || (!allow_directory && value.trim().is_empty()) {
        return Err(MarkdownWorkspaceError::InvalidRelativePath);
    }
    // Dot, parent, prefix, and root components are excluded even if canonicalization could normalize them.
    if path.components().any(|component| {
        // A single special component invalidates the whole untrusted path.
        !matches!(component, Component::Normal(_))
    }) {
        return Err(MarkdownWorkspaceError::InvalidRelativePath);
    }
    Ok(path)
}

/// Reserve the first path component case-insensitively, including on case-sensitive volumes.
pub(super) fn targets_reserved_attachments_tree(relative_path: &Path) -> bool {
    relative_path.components().next().is_some_and(|component| {
        // Only the top-level asset directory is reserved; similarly named nested user folders are allowed.
        let Component::Normal(name) = component else {
            // Invalid syntax is rejected separately by path validation rather than classified as assets.
            return false;
        };
        name.to_str().is_some_and(|name| {
            // Reserve the asset namespace consistently even on case-sensitive filesystems.
            name.eq_ignore_ascii_case(ATTACHMENTS_DIRECTORY_NAME)
        })
    })
}

/// Inspect each existing path component without following symbolic links.
pub(super) fn markdown_path_contains_symlink(canonical_root: &Path, relative_path: &Path) -> bool {
    let mut candidate = canonical_root.to_path_buf();
    // Check ancestors as well as the final entry; callers separately validate existence and type.
    for component in relative_path.components() {
        let Component::Normal(name) = component else {
            // Treat unexpected special components as unsafe rather than traversing them.
            return true;
        };
        candidate.push(name);
        match fs::symlink_metadata(&candidate) {
            // Any redirect violates the session's no-symlink access contract.
            Ok(metadata) if metadata.file_type().is_symlink() => return true,
            // A real existing component is safe to extend for the next inspection.
            Ok(_) => {}
            // Let the resolver report a missing or unreadable candidate with its specific public error.
            Err(_) => return false,
        }
    }
    false
}

/// Enforce containment on canonical paths using component-aware prefix matching.
pub(super) fn ensure_within_root(
    canonical_root: &Path,
    candidate: &Path,
) -> Result<(), MarkdownWorkspaceError> {
    // The root itself and its descendants share the session boundary.
    if candidate == canonical_root || candidate.starts_with(canonical_root) {
        Ok(())
    } else {
        // Refuse publication of paths outside the selected vault.
        Err(MarkdownWorkspaceError::PathEscapesRoot)
    }
}

/// Produce a vault-relative note identifier after validating its Markdown extension.
pub(super) fn relative_markdown_path(
    canonical_root: &Path,
    path: &Path,
) -> Result<String, MarkdownWorkspaceError> {
    // Reject non-note targets before exposing them through the note API.
    if !has_markdown_extension(path) {
        return Err(MarkdownWorkspaceError::NotMarkdown);
    }
    relative_workspace_path(canonical_root, path)
}

/// Convert a contained path to the UTF-8 identifier used by the public session API.
pub(super) fn relative_workspace_path(
    canonical_root: &Path,
    path: &Path,
) -> Result<String, MarkdownWorkspaceError> {
    path.strip_prefix(canonical_root)
        .map_err(|_| {
            // A path without the canonical vault prefix cannot become a public relative identifier.
            MarkdownWorkspaceError::PathEscapesRoot
        })?
        .to_str()
        .map(str::to_owned)
        .ok_or(MarkdownWorkspaceError::UnsupportedFileName)
}

/// Match the supported note suffix without requiring a filename allocation.
pub(super) fn has_markdown_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| {
            // Only a UTF-8 suffix can participate in supported note or asset type recognition.
            extension.to_str()
        })
        .is_some_and(|extension| {
            // Recognize the supported Markdown suffix without allocating a lowercase filename.
            extension.eq_ignore_ascii_case("md")
        })
}

/// Preserve the UTF-8 filename stem as the user-visible note title.
pub(super) fn markdown_title(path: &Path) -> Result<String, MarkdownWorkspaceError> {
    path.file_stem()
        .and_then(|stem| {
            // A public note or asset identifier requires a UTF-8 filename stem that can round-trip through the UI.
            stem.to_str()
        })
        .map(str::to_owned)
        .ok_or(MarkdownWorkspaceError::UnsupportedFileName)
}

/// Validate one filename component and normalize an optional lowercase .md suffix.
pub(super) fn validated_note_title(requested_title: &str) -> Result<&str, MarkdownWorkspaceError> {
    let title = validate_entry_name(
        requested_title,
        MAX_NOTE_TITLE_CHARACTERS,
        MarkdownWorkspaceError::InvalidNoteTitle,
    )?;
    let title = title.strip_suffix(".md").unwrap_or(title).trim();
    // Removing the suffix and surrounding whitespace must still leave a bounded usable title.
    if title.is_empty() || title.chars().count() > MAX_NOTE_TITLE_CHARACTERS {
        return Err(MarkdownWorkspaceError::InvalidNoteTitle);
    }
    Ok(title)
}
