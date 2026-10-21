//! Regression tests preserved with their owning document/workspace boundary.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[cfg(feature = "native-markdown-candidate")]
use tempfile::TempDir;
use tempfile::tempdir;

#[cfg(feature = "native-markdown-candidate")]
use crate::markdown_document_buffer::MarkdownTextSelection;

#[cfg(feature = "native-markdown-candidate")]
use super::conflict_draft_path;
use super::{MAX_ATTACHMENT_BYTES, MarkdownWorkspace, MarkdownWorkspaceError};

/// Isolated owner for a vault and private application-data tree across workspace reconstruction.
#[cfg(feature = "native-markdown-candidate")]
struct ConflictRecoveryFixture {
    _temporary_directory: TempDir,
    vault_directory: PathBuf,
    recovery_directory: PathBuf,
}

#[cfg(feature = "native-markdown-candidate")]
impl ConflictRecoveryFixture {
    /// Own isolated vault and private application-data directories for conflict recovery tests.
    fn new() -> Self {
        // Keep one TempDir owner for both trees so recovery persistence can outlive workspace reconstruction within the test.
        let temporary_directory = tempdir().expect("conflict recovery fixture");
        let vault_directory = temporary_directory.path().join("vault");
        let recovery_directory = temporary_directory
            .path()
            .join("app-data/markdown-conflict-recovery");
        fs::create_dir(&vault_directory).expect("vault directory");
        Self {
            _temporary_directory: temporary_directory,
            vault_directory,
            recovery_directory,
        }
    }

    /// Reconstruct a workspace over the same fixture paths without sharing active-session state.
    fn workspace(&self) -> MarkdownWorkspace {
        // Reuse persisted paths while constructing fresh counters and activation ownership.
        MarkdownWorkspace::with_storage_locations(
            self.vault_directory.clone(),
            self.recovery_directory.clone(),
        )
    }

    /// Find the deterministic private record path used to inspect fixture persistence.
    fn draft_path(&self, relative_path: &str) -> PathBuf {
        // Use the production path derivation so corruption tests target the record for the intended note.
        conflict_draft_path(&self.recovery_directory, relative_path)
    }
}

#[test]
/// Verify configured workspace state stays inactive until explicit activation and closes synchronously.
fn session_opens_only_on_demand_and_releases_state() {
    // A new workspace is path configuration only and owns no active root marker.
    let root = tempdir().expect("temporary root");
    let workspace = MarkdownWorkspace::with_root(root.path().join("notes"));
    assert!(!workspace.is_session_open());

    // Explicit activation creates the vault and allows an empty metadata listing.
    let access = workspace.open_session().expect("open session");
    assert!(workspace.is_session_open());
    let snapshot = access.list_workspace().expect("empty workspace");
    assert!(snapshot.notes.is_empty());
    assert!(snapshot.directories.is_empty());

    // Closing clears activation and prevents acquiring another active-session handle.
    workspace.close_session();
    assert!(!workspace.is_session_open());
    assert_eq!(
        workspace.active_session().err(),
        Some(MarkdownWorkspaceError::SessionClosed)
    );
}

#[cfg(feature = "native-markdown-candidate")]
#[test]
/// Keep the native mmap editor behind the same vault path validation as other note operations.
fn session_opens_native_document_buffer_only_after_vault_confinement() {
    // Create one Unicode note and explicitly activate its vault.
    let root = tempdir().expect("temporary root");
    fs::write(root.path().join("Note.md"), "# 标题\n正文 😀").expect("native buffer fixture");
    let workspace = MarkdownWorkspace::with_root(root.path().to_path_buf());
    let access = workspace.open_session().expect("open session");

    // The native buffer must stream exact bytes with no edit allocation and reject a parent-traversal identifier.
    let buffer = access
        .open_document_buffer("Note.md")
        .expect("open confined native buffer");
    let mut bytes = Vec::new();
    buffer.write_to(&mut bytes).expect("stream native buffer");
    assert_eq!(String::from_utf8(bytes).unwrap(), "# 标题\n正文 😀");
    assert_eq!(buffer.storage_profile().appended_edit_bytes, 0);
    assert_eq!(
        access.open_document_buffer("../Note.md").err(),
        Some(MarkdownWorkspaceError::InvalidRelativePath)
    );
}

#[cfg(feature = "native-markdown-candidate")]
#[test]
/// Verify native persistence commits exact edited bytes without erasing canonical editing history.
fn native_document_save_streams_pieces_and_preserves_edit_history() {
    // Create a live dirty piece tree and retain a Unicode selection across the save boundary.
    let root = tempdir().expect("temporary root");
    fs::write(root.path().join("Note.md"), "before").expect("native save fixture");
    let workspace = MarkdownWorkspace::with_root(root.path().to_path_buf());
    let access = workspace.open_session().expect("open session");
    let mut document = access
        .open_document_buffer("Note.md")
        .expect("open native document");
    let replacement = "# 保存后\n正文 😀";
    let replacement_utf16_length = replacement.encode_utf16().count();
    document
        .replace_utf16_range(
            0.."before".len(),
            replacement,
            MarkdownTextSelection::caret(replacement_utf16_length),
        )
        .expect("edit native document");
    assert!(document.is_dirty());
    assert!(document.storage_profile().appended_edit_bytes > 0);

    // An explicit native save must stream and atomically publish the live document.
    let receipt = access
        .save_document_buffer(&mut document)
        .expect("stream native document save");

    // Compare disk bytes, stable revision, selection, clean state, retained history, and temporary-file cleanup.
    assert_eq!(
        fs::read_to_string(root.path().join("Note.md")).unwrap(),
        replacement
    );
    assert_eq!(
        receipt.revision,
        document
            .original_revision()
            .expect("normal saved disk baseline")
            .encode_hex()
    );
    assert_eq!(receipt.byte_count, replacement.len() as u64);
    assert!(!document.is_dirty());
    assert_eq!(
        document.path(),
        root.path()
            .join("Note.md")
            .canonicalize()
            .expect("canonical saved note")
    );
    assert_eq!(document.selection().head_utf16, replacement_utf16_length);
    assert_eq!(
        document.storage_profile().appended_edit_bytes,
        replacement.len()
    );
    assert!(document.storage_profile().allocated_edit_segment_bytes > 0);
    assert_eq!(document.storage_profile().undo_entries, 1);
    assert_eq!(document.storage_profile().redo_entries, 0);
    assert!(fs::read_dir(root.path()).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".omni-md-save-")
    }));
}

#[cfg(feature = "native-markdown-candidate")]
#[test]
/// Preserve external bytes and native unsaved edits without allocating legacy recovery storage.
fn native_document_conflict_keeps_only_the_live_piece_tree_draft() {
    // Create local piece-tree edits, then independently replace the authoritative file with external text.
    let fixture = ConflictRecoveryFixture::new();
    fs::write(fixture.vault_directory.join("Note.md"), "before").expect("native conflict fixture");
    let workspace = fixture.workspace();
    let access = workspace.open_session().expect("open session");
    let mut document = access
        .open_document_buffer("Note.md")
        .expect("open native document");
    let draft = "local 中文 😀";
    document
        .replace_utf16_range(
            0.."before".len(),
            draft,
            MarkdownTextSelection::caret(draft.encode_utf16().count()),
        )
        .expect("edit native draft");
    fs::write(fixture.vault_directory.join("Note.md"), "external").expect("external replacement");

    // A save conflict must preserve both versions, dirty state, and history while leaving private recovery storage absent.
    assert_eq!(
        access.save_document_buffer(&mut document).err(),
        Some(MarkdownWorkspaceError::SaveConflict)
    );
    assert_eq!(
        fs::read_to_string(fixture.vault_directory.join("Note.md")).unwrap(),
        "external"
    );
    let mut draft_bytes = Vec::new();
    document
        .write_to(&mut draft_bytes)
        .expect("stream retained native draft");
    assert_eq!(String::from_utf8(draft_bytes).unwrap(), draft);
    assert!(document.is_dirty());
    assert!(!fixture.recovery_directory.exists());
    assert!(
        fs::read_dir(&fixture.vault_directory)
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".omni-md-save-"))
    );
}

#[cfg(feature = "native-markdown-candidate")]
#[test]
/// Preserve live TextKit text when persistence observes a different external revision.
fn native_document_without_local_edits_rejects_hidden_external_text_replacement() {
    // A live TextKit owner can have cached attributes even when no local edits exist.
    let root = tempdir().expect("temporary root");
    fs::write(root.path().join("Note.md"), "before").expect("initial disk document");
    let workspace = MarkdownWorkspace::with_root(root.path().to_path_buf());
    let access = workspace.open_session().expect("open session");
    let mut document = access
        .open_document_buffer("Note.md")
        .expect("open native document");
    document
        .set_selection(MarkdownTextSelection::caret(4))
        .unwrap();
    let generation = document.generation();
    let selection = document.selection();
    let revision = document.original_revision();
    fs::write(root.path().join("Note.md"), "external shorter 😀").unwrap();

    // Save reports conflict rather than changing text/length or resetting generation behind TextKit.
    assert_eq!(
        access.save_document_buffer(&mut document),
        Err(MarkdownWorkspaceError::SaveConflict)
    );
    let mut live = Vec::new();
    document.write_to(&mut live).unwrap();
    assert_eq!(live, b"before");
    assert_eq!(document.selection(), selection);
    assert_eq!(document.generation(), generation);
    assert_eq!(document.original_revision(), revision);
    assert!(!document.is_dirty());
    assert!(!document.can_undo());
    assert_eq!(
        fs::read_to_string(root.path().join("Note.md")).unwrap(),
        "external shorter 😀"
    );
}

#[test]
/// Keep note discovery confined while excluding reserved assets and symlink escapes.
fn discovery_stays_inside_root_and_excludes_attachments_and_symlinks() {
    // Mix supported notes, ignored extensions, a reserved asset tree, and a link outside the vault.
    let root = tempdir().expect("temporary root");
    fs::create_dir(root.path().join("topic")).expect("topic directory");
    fs::create_dir(root.path().join("attachments")).expect("attachments directory");
    fs::write(root.path().join("Root.md"), "root").expect("root note");
    fs::write(root.path().join("topic/Child.MD"), "child").expect("child note");
    fs::write(root.path().join("topic/ignored.txt"), "ignored").expect("text file");
    fs::write(root.path().join("attachments/hidden.md"), "hidden").expect("attachment");
    std::os::unix::fs::symlink(root.path().join("Root.md"), root.path().join("linked.md"))
        .expect("note symlink");

    // Listing returns only canonical supported note/directory metadata and never traverses the link.
    let workspace = MarkdownWorkspace::with_root(root.path().to_path_buf());
    let snapshot = workspace
        .open_session()
        .expect("open session")
        .list_workspace()
        .expect("list workspace");
    let paths = snapshot
        .notes
        .into_iter()
        .map(|note| {
            // Compare public relative identities without retaining or opening any note body.
            note.relative_path
        })
        .collect::<Vec<_>>();
    assert_eq!(paths, vec!["Root.md", "topic/Child.MD"]);
    assert_eq!(snapshot.directories, vec!["topic"]);
}

#[test]
/// Unrelated files must consume scan budget even though they produce no visible note rows.
fn bounded_workspace_walk_counts_ignored_entries_against_scan_budget() {
    let root = tempdir().expect("temporary root");
    for index in 0..32 {
        // More ignored files than the budget expose a walker that charges only accepted Markdown.
        fs::write(
            root.path().join(format!("ignored-{index:02}.txt")),
            b"ignored",
        )
        .expect("ignored fixture");
    }
    let mut visited_entries = 0_usize;
    let scanned_entries = super::walk_markdown_workspace_bounded(root.path(), 7, |_| {
        // The callback must never see ignored files; its count is distinct from charged scan work.
        visited_entries += 1;
        Ok(super::WalkControl::Continue)
    })
    .expect("bounded workspace walk");
    assert_eq!(scanned_entries, 7);
    assert_eq!(visited_entries, 0);
}

#[cfg(feature = "native-markdown-candidate")]
#[test]
/// Bound native sidebar discovery independently of note encoding or body size.
fn native_workspace_listing_bounds_metadata_without_opening_note_bodies() {
    // Use an invalid-UTF-8 note to detect accidental body reads during metadata listing.
    let root = tempdir().expect("temporary root");
    fs::write(root.path().join("Unopened.md"), [0xff, 0xfe])
        .expect("invalid unopened note fixture");
    let workspace = MarkdownWorkspace::with_root(root.path().to_path_buf());
    let access = workspace.open_session().expect("open session");

    // A one-entry metadata budget must return the file without decoding its invalid body.
    let invalid_only = access
        .list_workspace_bounded(1, 1)
        .expect("metadata-only bounded listing");
    assert_eq!(invalid_only.directories.len() + invalid_only.notes.len(), 1);
    assert_eq!(invalid_only.notes[0].relative_path, "Unopened.md");

    // Adding a directory must still respect independent output and scan bounds.
    fs::create_dir(root.path().join("topic")).expect("topic directory");
    fs::write(root.path().join("Current.md"), "# Current\n正文").expect("current note fixture");
    let bounded = access
        .list_workspace_bounded(2, 32)
        .expect("combined bounded listing");
    assert_eq!(bounded.directories.len() + bounded.notes.len(), 2);
}

#[cfg(feature = "native-markdown-candidate")]
#[test]
/// Keep a damaged unrelated note from preventing listing or opening a valid current note.
fn workspace_listing_and_current_note_open_do_not_read_other_note_bodies() {
    // Create a valid current note beside an invalid-UTF-8 sibling.
    let root = tempdir().expect("temporary root");
    fs::write(root.path().join("Current.md"), "# Current\n正文").expect("current note fixture");
    fs::write(root.path().join("Unopened.md"), [0xff, 0xfe])
        .expect("invalid unopened note fixture");

    // Listing must succeed because it reads file metadata only.
    let workspace = MarkdownWorkspace::with_root(root.path().to_path_buf());
    let access = workspace.open_session().expect("open session");
    let snapshot = access
        .list_workspace()
        .expect("metadata-only workspace listing");
    assert_eq!(
        snapshot
            .notes
            .iter()
            .map(|note| note.relative_path.as_str())
            .collect::<Vec<_>>(),
        vec!["Current.md", "Unopened.md"]
    );

    // Opening the valid note must remain independent of the damaged sibling body.
    let current = access
        .open_document_buffer("Current.md")
        .expect("open current note");
    // Only this finite synthetic oracle owns output bytes; the public buffer remains streamed.
    let mut bytes = Vec::new();
    current.write_to(&mut bytes).expect("stream current note");
    assert_eq!(bytes, "# Current\n正文".as_bytes());
    assert_eq!(
        access.open_document_buffer("Unopened.md").err(),
        Some(MarkdownWorkspaceError::InvalidUtf8Document)
    );
}

#[cfg(feature = "native-markdown-candidate")]
#[test]
/// A successful streamed native save must not create historical conflict storage.
fn normal_save_never_creates_conflict_recovery_storage() {
    let fixture = ConflictRecoveryFixture::new();
    fs::write(fixture.vault_directory.join("note.md"), "before").expect("note fixture");
    let workspace = fixture.workspace();
    let access = workspace.open_session().expect("open session");
    let mut document = access
        .open_document_buffer("note.md")
        .expect("open native buffer");
    // Publish real canonical edits before saving; the save cannot obtain text from a legacy String DTO.
    document
        .replace_utf16_range(
            0..document.utf16_len(),
            "saved",
            MarkdownTextSelection::caret(5),
        )
        .unwrap();
    access
        .save_document_buffer(&mut document)
        .expect("streamed save");
    assert_eq!(
        fs::read(fixture.vault_directory.join("note.md")).unwrap(),
        b"saved"
    );
    assert!(!fixture.recovery_directory.exists());
}

#[cfg(feature = "native-markdown-candidate")]
#[test]
/// Actual mapped recovery rejects linked directories and records without changing their targets.
fn conflict_recovery_rejects_directory_and_file_symlinks() {
    let fixture = ConflictRecoveryFixture::new();
    fs::write(fixture.vault_directory.join("note.md"), "disk").unwrap();
    let workspace = fixture.workspace();
    let access = workspace.open_session().unwrap();
    access.write_conflict_fixture("note.md", "local").unwrap();
    let original = fs::read(fixture.draft_path("note.md")).unwrap();
    let outside = tempdir().unwrap();
    let moved = outside.path().join("records");
    // The actual record is valid; only its directory role changes before the public open.
    fs::rename(&fixture.recovery_directory, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &fixture.recovery_directory).unwrap();
    assert_eq!(
        access.open_document_buffer("note.md").err(),
        Some(MarkdownWorkspaceError::ConflictRecoveryUnavailable)
    );
    fs::remove_file(&fixture.recovery_directory).unwrap();
    fs::rename(&moved, &fixture.recovery_directory).unwrap();
    assert_eq!(fs::read(fixture.draft_path("note.md")).unwrap(), original);
    let external = outside.path().join("outside.draft");
    fs::write(&external, &original).unwrap();
    fs::set_permissions(&external, fs::Permissions::from_mode(0o600)).unwrap();
    fs::remove_file(fixture.draft_path("note.md")).unwrap();
    std::os::unix::fs::symlink(&external, fixture.draft_path("note.md")).unwrap();
    assert_eq!(
        access.open_document_buffer("note.md").err(),
        Some(MarkdownWorkspaceError::ConflictDraftCorrupted)
    );
    assert_eq!(fs::read(&external).unwrap(), original);
    assert_eq!(
        fs::read(fixture.vault_directory.join("note.md")).unwrap(),
        b"disk"
    );
}

#[cfg(feature = "native-markdown-candidate")]
#[test]
/// Avoid silently hiding unsaved recovery corruption behind otherwise readable disk text.
fn malformed_conflict_draft_is_rejected_without_falling_back_to_disk() {
    // Write an owner-private record with malformed encoded content over a valid note's draft path.
    let fixture = ConflictRecoveryFixture::new();
    fs::write(fixture.vault_directory.join("note.md"), "disk").expect("note fixture");
    fs::create_dir_all(&fixture.recovery_directory).expect("recovery directory");
    fs::set_permissions(
        &fixture.recovery_directory,
        fs::Permissions::from_mode(0o700),
    )
    .expect("private directory permissions");
    let draft_path = fixture.draft_path("note.md");
    fs::write(&draft_path, b"not a recovery draft").expect("corrupt draft");
    fs::set_permissions(&draft_path, fs::Permissions::from_mode(0o600))
        .expect("private file permissions");
    let workspace = fixture.workspace();
    let access = workspace.open_session().expect("open session");

    // Opening the note must surface record corruption and preserve the original disk text.
    assert_eq!(
        access.open_document_buffer("note.md").err(),
        Some(MarkdownWorkspaceError::ConflictDraftCorrupted)
    );
    assert_eq!(
        fs::read_to_string(fixture.vault_directory.join("note.md")).unwrap(),
        "disk"
    );
}

#[test]
/// Keep note creation and bounded title/body search within supported Markdown entries.
fn create_and_search_use_only_markdown_files() {
    // Create supported notes beside ignored extensions and verify collision-safe note creation.
    let root = tempdir().expect("temporary root");
    let workspace = MarkdownWorkspace::with_root(root.path().to_path_buf());
    let access = workspace.open_session().expect("open session");
    access
        .create_directory("", "Projects")
        .expect("create directory");
    let created = access
        .create_note("Projects", "Roadmap")
        .expect("create note");
    let content = "# Roadmap\n\n极致性能目标";
    // This case owns search fixtures, not editor behavior: populate the real newly created
    // file directly so basic workspace search remains testable without native editor features.
    fs::write(root.path().join(&created.relative_path), content).expect("search body fixture");
    fs::write(root.path().join("ignored.txt"), "极致性能目标").expect("ignored file");

    // ASCII-insensitive title matching must find the expected note before body results.
    let title_results = access.search_notes("roadmap", 10).expect("title search");
    assert_eq!(title_results.len(), 1);
    assert_eq!(title_results[0].relative_path, "Projects/Roadmap.md");
    assert!(title_results[0].snippet.is_none());

    // Unicode body search must return a bounded relevant snippet without exposing ignored files.
    let body_results = access.search_notes("极致性能", 10).expect("body search");
    assert_eq!(body_results.len(), 1);
    assert!(
        body_results[0]
            .snippet
            .as_deref()
            .unwrap()
            .contains("极致性能")
    );
}

#[test]
/// Preserve an existing vault note while importing exact Unicode source under a collision suffix.
fn markdown_import_uses_mmap_content_and_never_overwrites() {
    // Give the external source the same title as an already populated destination.
    let root = tempdir().expect("temporary root");
    let external = tempdir().expect("external source root");
    fs::write(root.path().join("Guide.md"), "existing").expect("collision fixture");
    fs::write(external.path().join("Guide.md"), "# imported\n\nbody").expect("import fixture");
    let workspace = MarkdownWorkspace::with_root(root.path().to_path_buf());
    let access = workspace.open_session().expect("open session");

    // Import through validated mapped inspection and an exclusive destination candidate.
    let imported = access
        .import_note(&external.path().join("Guide.md"), "")
        .expect("import note");

    // The new suffix, exact bytes, and untouched existing destination prove nonoverwriting behavior.
    assert_eq!(imported.relative_path, "Guide 2.md");
    assert_eq!(
        fs::read_to_string(root.path().join("Guide.md")).unwrap(),
        "existing"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("Guide 2.md")).unwrap(),
        "# imported\n\nbody"
    );
}

#[test]
/// Reject invalid Markdown encoding before publishing an imported note.
fn invalid_utf8_markdown_import_leaves_no_file() {
    // Create invalid external bytes and an otherwise empty destination vault.
    let root = tempdir().expect("temporary root");
    let external = tempdir().expect("external source root");
    fs::write(external.path().join("Broken.md"), [0xff, 0xfe]).expect("invalid UTF-8 fixture");
    let workspace = MarkdownWorkspace::with_root(root.path().to_path_buf());
    let access = workspace.open_session().expect("open session");

    // Import must return the encoding error and leave the vault without a partial file.
    assert_eq!(
        access
            .import_note(&external.path().join("Broken.md"), "")
            .err(),
        Some(MarkdownWorkspaceError::InvalidUtf8Document)
    );
    assert!(fs::read_dir(root.path()).unwrap().next().is_none());
}

#[test]
/// Keep attachment bytes exact while safely encoding nested image insertion Markdown.
fn attachment_import_streams_to_reserved_directory_and_builds_nested_link() {
    // Create a nested note, a punctuation-bearing external image filename, and an existing collision.
    let root = tempdir().expect("temporary root");
    let external = tempdir().expect("external source root");
    fs::create_dir(root.path().join("Projects")).expect("nested notes directory");
    fs::create_dir(root.path().join("attachments")).expect("attachments directory");
    fs::write(root.path().join("Projects/Plan.md"), "plan").expect("note fixture");
    fs::write(
        root.path().join("attachments/diagram (v1)#.PNG"),
        "existing",
    )
    .expect("attachment collision");
    fs::write(external.path().join("diagram (v1)#.PNG"), b"image bytes")
        .expect("attachment fixture");
    let workspace = MarkdownWorkspace::with_root(root.path().to_path_buf());
    let access = workspace.open_session().expect("open session");

    // Import the asset through the fixed-buffer copy path.
    let imported = access
        .import_attachment(
            "Projects/Plan.md",
            &external.path().join("diagram (v1)#.PNG"),
        )
        .expect("import attachment");

    // The result must preserve both existing data and exact source bytes while producing an escaped nested relative image link.
    assert_eq!(
        imported.attachment_relative_path,
        "attachments/diagram (v1)# 2.PNG"
    );
    assert_eq!(
        imported.markdown_insertion,
        "![diagram (v1)# 2.PNG](../attachments/diagram%20%28v1%29%23%202.PNG)"
    );
    assert_eq!(
        fs::read(root.path().join(&imported.attachment_relative_path)).unwrap(),
        b"image bytes"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("attachments/diagram (v1)#.PNG")).unwrap(),
        "existing"
    );
}

#[cfg(feature = "native-markdown-candidate")]
#[test]
/// Preserve literal code and unrelated links while updating imported asset links after a depth-changing move.
fn moving_note_rewrites_only_live_vault_attachment_destinations() {
    // Mix live attachment links with fenced code, inline code, escapes, and external destinations.
    let root = tempdir().expect("temporary root");
    fs::create_dir(root.path().join("Archive")).expect("archive directory");
    let original = concat!(
        "![diagram](attachments/diagram.png)\n",
        "[manual](attachments/manual%20v1.pdf)\n",
        "`[inline sample](attachments/unchanged.png)`\n",
        "```md\n",
        "![fenced sample](attachments/unchanged.png)\n",
        "```\n",
    );
    fs::write(root.path().join("Note.md"), original).expect("note fixture");
    let workspace = MarkdownWorkspace::with_root(root.path().to_path_buf());
    let access = workspace.open_session().expect("open session");
    let opened = access.open_document_buffer("Note.md").expect("open note");
    let opened_revision = opened.original_revision().unwrap().encode_hex();

    // Move to a nested directory and compare only the intended imported-link prefix changes.
    let moved = access
        .move_note("Note.md", "Archive", Some(&opened_revision))
        .expect("move nested");
    let nested = fs::read_to_string(root.path().join("Archive/Note.md")).unwrap();
    assert_eq!(
        nested,
        concat!(
            "![diagram](../attachments/diagram.png)\n",
            "[manual](../attachments/manual%20v1.pdf)\n",
            "`[inline sample](attachments/unchanged.png)`\n",
            "```md\n",
            "![fenced sample](attachments/unchanged.png)\n",
            "```\n",
        )
    );

    // Move back to the root and require exact original source restoration.
    let moved_back = access
        .move_note("Archive/Note.md", "", Some(&moved.revision))
        .expect("move to root");
    assert_eq!(moved_back.relative_path, "Note.md");
    assert_eq!(
        fs::read_to_string(root.path().join("Note.md")).unwrap(),
        original
    );
}

#[test]
/// Reject unsafe attachment size and reserved-directory redirection before copying source bytes.
fn attachment_import_rejects_oversize_and_symlink_destinations() {
    // Create a sparse external file beyond the asset byte limit without allocating that body in memory.
    let root = tempdir().expect("temporary root");
    let external = tempdir().expect("external source root");
    fs::write(root.path().join("Note.md"), "note").expect("note fixture");
    let oversized = external.path().join("oversized.bin");
    fs::File::create(&oversized)
        .and_then(|file| {
            // Create a sparse oversized fixture without allocating or copying a huge attachment body.
            file.set_len(MAX_ATTACHMENT_BYTES + 1)
        })
        .expect("sparse oversized fixture");
    let workspace = MarkdownWorkspace::with_root(root.path().to_path_buf());
    let access = workspace.open_session().expect("open session");

    // The size failure must leave the reserved attachment directory absent.
    assert_eq!(
        access.import_attachment("Note.md", &oversized).err(),
        Some(MarkdownWorkspaceError::AttachmentTooLarge)
    );
    assert!(!root.path().join("attachments").exists());

    // A linked asset directory must reject import and preserve the outside destination tree.
    let outside_destination = tempdir().expect("outside attachment destination");
    std::os::unix::fs::symlink(outside_destination.path(), root.path().join("attachments"))
        .expect("reserved directory symlink");
    let source = external.path().join("small.bin");
    fs::write(&source, "small").expect("small attachment fixture");
    assert_eq!(
        access.import_attachment("Note.md", &source).err(),
        Some(MarkdownWorkspaceError::StorageUnavailable)
    );
    assert!(
        fs::read_dir(outside_destination.path())
            .unwrap()
            .next()
            .is_none()
    );
}

#[cfg(feature = "native-markdown-candidate")]
#[test]
/// Keep imported asset storage outside user note and destination APIs.
fn reserved_attachments_tree_cannot_be_opened_as_notes_or_note_directories() {
    // Place a Markdown-suffixed file inside the reserved asset directory.
    let root = tempdir().expect("temporary root");
    fs::create_dir(root.path().join("attachments")).expect("attachments directory");
    fs::write(root.path().join("attachments/hidden.md"), "hidden")
        .expect("hidden markdown attachment");
    let workspace = MarkdownWorkspace::with_root(root.path().to_path_buf());
    let access = workspace.open_session().expect("open session");

    // Note opening and directory resolution must both reject this reserved subtree.
    assert_eq!(
        access.open_document_buffer("attachments/hidden.md").err(),
        Some(MarkdownWorkspaceError::NoteNotFound)
    );
    assert_eq!(
        access.create_note("attachments", "Not allowed").err(),
        Some(MarkdownWorkspaceError::DirectoryNotFound)
    );
}

#[test]
/// Reject external import links and directory sources without following their targets.
fn import_sources_must_be_regular_files_not_symlinks() {
    // Prepare real sources beside symlinked note and asset paths plus a directory.
    let root = tempdir().expect("temporary root");
    let external = tempdir().expect("external source root");
    fs::write(root.path().join("Note.md"), "note").expect("note fixture");
    fs::write(external.path().join("Source.md"), "source").expect("source fixture");
    std::os::unix::fs::symlink(
        external.path().join("Source.md"),
        external.path().join("Linked.md"),
    )
    .expect("source symlink");
    let workspace = MarkdownWorkspace::with_root(root.path().to_path_buf());
    let access = workspace.open_session().expect("open session");

    // All nonregular or redirected import sources must reject without creating vault data.
    assert_eq!(
        access
            .import_note(&external.path().join("Linked.md"), "")
            .err(),
        Some(MarkdownWorkspaceError::ImportSourceUnavailable)
    );
    assert_eq!(
        access
            .import_attachment("Note.md", &external.path().join("Linked.md"))
            .err(),
        Some(MarkdownWorkspaceError::ImportSourceUnavailable)
    );
    assert!(!root.path().join("Linked.md").exists());
    assert!(!root.path().join("attachments").exists());
}

#[cfg(feature = "native-markdown-candidate")]
#[test]
/// Reject lexical parent traversal and filesystem-link escapes at the note boundary.
fn traversal_and_symlink_escape_are_rejected() {
    // Place a real outside note beside a confined vault and add a symlink escape inside it.
    let parent = tempdir().expect("temporary parent");
    let root = parent.path().join("root");
    fs::create_dir(&root).expect("root directory");
    fs::write(parent.path().join("outside.md"), "outside").expect("outside note");
    std::os::unix::fs::symlink(parent.path().join("outside.md"), root.join("escape.md"))
        .expect("escape symlink");
    let workspace = MarkdownWorkspace::with_root(root);
    let access = workspace.open_session().expect("open session");

    // Both traversal syntax and a linked note must reject before returning outside contents.
    assert_eq!(
        access.open_document_buffer("../outside.md").err(),
        Some(MarkdownWorkspaceError::InvalidRelativePath)
    );
    assert_eq!(
        access.open_document_buffer("escape.md").err(),
        Some(MarkdownWorkspaceError::NoteNotFound)
    );
}

#[test]
/// Support explicit nonempty user-subtree rename and delete without replacing other directories.
fn directory_lifecycle_preserves_non_empty_trees_and_rejects_collisions() {
    // Create a user subtree with a note alongside an occupied destination name.
    let root = tempdir().expect("temporary root");
    fs::create_dir_all(root.path().join("Projects/Omni")).expect("nested directory fixture");
    fs::write(root.path().join("Projects/Omni/Plan.md"), "plan").expect("note fixture");
    fs::create_dir(root.path().join("Taken")).expect("collision fixture");
    let workspace = MarkdownWorkspace::with_root(root.path().to_path_buf());
    let access = workspace.open_session().expect("open session");

    // A rename collision must preserve both source contents and the occupied directory.
    assert_eq!(
        access.rename_directory("Projects", "Taken").err(),
        Some(MarkdownWorkspaceError::EntryAlreadyExists)
    );
    assert_eq!(
        fs::read_to_string(root.path().join("Projects/Omni/Plan.md")).unwrap(),
        "plan"
    );

    // A successful rename must preserve the nonempty subtree and return its new identifier.
    let renamed = access
        .rename_directory("Projects", "Work")
        .expect("rename non-empty directory");
    assert_eq!(renamed, "Work");
    assert_eq!(
        fs::read_to_string(root.path().join("Work/Omni/Plan.md")).unwrap(),
        "plan"
    );
    assert!(!root.path().join("Projects").exists());

    // Explicit deletion removes the selected renamed subtree and leaves unrelated entries intact.
    access
        .delete_directory("Work")
        .expect("delete non-empty directory");
    assert!(!root.path().join("Work").exists());
    assert!(root.path().join("Taken").is_dir());
}

#[test]
/// Protect the session root and reserved asset namespace across directory mutations.
fn directory_lifecycle_protects_root_attachments_and_relative_path_boundary() {
    // Create only an active vault plus its reserved asset directory.
    let root = tempdir().expect("temporary root");
    fs::create_dir(root.path().join("attachments")).expect("reserved directory fixture");
    fs::create_dir(root.path().join("Folder")).expect("directory fixture");
    let workspace = MarkdownWorkspace::with_root(root.path().to_path_buf());
    let access = workspace.open_session().expect("open session");

    // Root rename/delete, reserved destination names, and invalid relative paths must reject without mutating either boundary.
    assert_eq!(
        access.rename_directory("", "Renamed").err(),
        Some(MarkdownWorkspaceError::RootDirectoryProtected)
    );
    assert_eq!(
        access.delete_directory("").err(),
        Some(MarkdownWorkspaceError::RootDirectoryProtected)
    );
    assert_eq!(
        access.create_directory("", "ATTACHMENTS").err(),
        Some(MarkdownWorkspaceError::ReservedDirectoryName)
    );
    assert_eq!(
        access.rename_directory("Folder", "attachments").err(),
        Some(MarkdownWorkspaceError::ReservedDirectoryName)
    );
    assert_eq!(
        access.delete_directory("attachments").err(),
        Some(MarkdownWorkspaceError::DirectoryNotFound)
    );
    assert_eq!(
        access.delete_directory("../Folder").err(),
        Some(MarkdownWorkspaceError::InvalidRelativePath)
    );
    assert_eq!(
        access
            .delete_directory(root.path().to_string_lossy().as_ref())
            .err(),
        Some(MarkdownWorkspaceError::InvalidRelativePath)
    );
    assert!(root.path().join("Folder").is_dir());
    assert!(root.path().join("attachments").is_dir());
}

#[test]
/// Prevent recursive user-directory deletion from following links into outside data.
fn directory_lifecycle_rejects_symlink_targets_and_does_not_follow_nested_symlinks() {
    // Create an outside sentinel and links both as selected directories and inside a real subtree.
    let root = tempdir().expect("temporary root");
    let outside = tempdir().expect("outside directory");
    let outside_file = outside.path().join("keep.md");
    fs::write(&outside_file, "outside").expect("outside file fixture");
    std::os::unix::fs::symlink(outside.path(), root.path().join("linked-directory"))
        .expect("target symlink fixture");
    fs::create_dir(root.path().join("Disposable")).expect("deletion fixture");
    std::os::unix::fs::symlink(outside.path(), root.path().join("Disposable/outside-link"))
        .expect("nested symlink fixture");
    let workspace = MarkdownWorkspace::with_root(root.path().to_path_buf());
    let access = workspace.open_session().expect("open session");

    // A selected directory symlink must reject; deleting the real subtree may remove its link but must not follow it.
    assert_eq!(
        access.delete_directory("linked-directory").err(),
        Some(MarkdownWorkspaceError::DirectoryNotFound)
    );
    access
        .delete_directory("Disposable")
        .expect("delete directory containing an external symlink");

    // The outside sentinel must survive with exact original bytes.
    assert_eq!(fs::read_to_string(&outside_file).unwrap(), "outside");
    assert!(outside.path().is_dir());
    assert!(!root.path().join("Disposable").exists());
}

#[test]
/// Allow explicit unopened-note actions without requiring an unavailable editor revision.
fn unopened_note_lifecycle_accepts_no_expected_revision() {
    // Create a supported note and destination directory without opening an editor.
    let root = tempdir().expect("temporary root");
    fs::create_dir(root.path().join("Archive")).expect("archive directory");
    fs::write(root.path().join("Inbox.md"), "body").expect("note fixture");
    let workspace = MarkdownWorkspace::with_root(root.path().to_path_buf());
    let access = workspace.open_session().expect("open session");

    // Rename, move, copy, and delete through optional-revision APIs using no optimistic precondition.
    let renamed = access
        .rename_note("Inbox.md", "Renamed", None)
        .expect("rename unopened note");
    let moved = access
        .move_note(&renamed.relative_path, "Archive", None)
        .expect("move unopened note");
    let copied = access
        .copy_note(&moved.relative_path, None)
        .expect("copy unopened note");
    access
        .delete_note(&copied.relative_path, None)
        .expect("delete unopened copy");

    // Returned identifiers and surviving contents must describe only the intended lifecycle results.
    assert_eq!(moved.relative_path, "Archive/Renamed.md");
    assert_eq!(
        fs::read_to_string(root.path().join(&moved.relative_path)).unwrap(),
        "body"
    );
    assert!(!root.path().join(&copied.relative_path).exists());
}

#[cfg(feature = "native-markdown-candidate")]
#[test]
/// Protect all existing entries while exercising successful revision-checked note lifecycle operations.
fn note_lifecycle_never_overwrites_existing_entries() {
    // Create a current note, an occupied title, and a destination directory.
    let root = tempdir().expect("temporary root");
    fs::create_dir(root.path().join("Archive")).expect("archive directory");
    fs::write(root.path().join("note.md"), "body").expect("note fixture");
    fs::write(root.path().join("taken.md"), "keep").expect("collision fixture");
    let workspace = MarkdownWorkspace::with_root(root.path().to_path_buf());
    let access = workspace.open_session().expect("open session");
    let opened = access.open_document_buffer("note.md").expect("open note");
    let opened_revision = opened.original_revision().unwrap().encode_hex();

    // A conflicting rename must preserve both notes without changing the original revision.
    assert_eq!(
        access
            .rename_note("note.md", "taken", Some(&opened_revision))
            .err(),
        Some(MarkdownWorkspaceError::EntryAlreadyExists)
    );
    assert_eq!(
        fs::read_to_string(root.path().join("taken.md")).unwrap(),
        "keep"
    );

    // A successful rename and move must retain the source bytes under the new identifier.
    let renamed = access
        .rename_note("note.md", "renamed.md", Some(&opened_revision))
        .expect("rename note");
    assert_eq!(renamed.relative_path, "renamed.md");
    let moved = access
        .move_note("renamed.md", "Archive", Some(&renamed.revision))
        .expect("move note");
    assert_eq!(moved.relative_path, "Archive/renamed.md");

    // A copy must receive an independent collision-safe path and identical bytes.
    let copied = access
        .copy_note(&moved.relative_path, Some(&moved.revision))
        .expect("copy note");
    assert_eq!(copied.relative_path, "Archive/renamed 副本.md");
    assert_eq!(
        fs::read_to_string(root.path().join(&copied.relative_path)).unwrap(),
        "body"
    );

    // Deleting the copy must preserve the moved original, after which explicit original deletion may remove it.
    access
        .delete_note(&copied.relative_path, Some(&copied.revision))
        .expect("delete copied note");
    assert!(!root.path().join(&copied.relative_path).exists());
    assert_eq!(
        fs::read_to_string(root.path().join(&moved.relative_path)).unwrap(),
        "body"
    );
}

#[cfg(feature = "native-markdown-candidate")]
#[test]
/// Require exact optimistic revisions before rename, move, copy, and delete of an opened note.
fn stale_revision_blocks_destructive_note_lifecycle_actions() {
    // Open one baseline and then replace its disk contents externally.
    let root = tempdir().expect("temporary root");
    fs::create_dir(root.path().join("Archive")).expect("archive directory");
    fs::write(root.path().join("note.md"), "before").expect("note fixture");
    let workspace = MarkdownWorkspace::with_root(root.path().to_path_buf());
    let access = workspace.open_session().expect("open session");
    let opened = access.open_document_buffer("note.md").expect("open note");
    let opened_revision = opened.original_revision().unwrap().encode_hex();
    fs::write(root.path().join("note.md"), "external").expect("external update");

    // Every stale-revision lifecycle operation must reject while preserving the external note and destination tree.
    assert_eq!(
        access
            .rename_note("note.md", "renamed", Some(&opened_revision))
            .err(),
        Some(MarkdownWorkspaceError::SaveConflict)
    );
    assert_eq!(
        access
            .move_note("note.md", "Archive", Some(&opened_revision))
            .err(),
        Some(MarkdownWorkspaceError::SaveConflict)
    );
    assert_eq!(
        access.copy_note("note.md", Some(&opened_revision)).err(),
        Some(MarkdownWorkspaceError::SaveConflict)
    );
    assert_eq!(
        access.delete_note("note.md", Some(&opened_revision)).err(),
        Some(MarkdownWorkspaceError::SaveConflict)
    );
    assert_eq!(
        fs::read_to_string(root.path().join("note.md")).unwrap(),
        "external"
    );
    assert!(!root.path().join("renamed.md").exists());
    assert!(!root.path().join("Archive/note.md").exists());
    assert!(!root.path().join("note 副本.md").exists());
}

/// Exercise mutable-source races in isolated processes so a future SIGBUS cannot kill unrelated tests.
#[test]
fn immutable_snapshot_external_mutation_isolated_regressions() {
    // Every scenario gets fresh paths and a single-test child, avoiding cross-test descriptor noise.
    let directory = tempdir().expect("snapshot process fixtures");
    #[cfg(not(feature = "native-markdown-candidate"))]
    let scenarios = ["inspection", "search", "release"];
    #[cfg(feature = "native-markdown-candidate")]
    let scenarios = ["inspection", "search", "recovery", "release"];
    for scenario in scenarios {
        // Run real external writers against the exact inode being inspected or retained by the child.
        let root = directory.path().join(scenario);
        fs::create_dir(&root).unwrap();
        let output = snapshot_test_command("immutable_snapshot_process_scenario")
            .env("OMNI_MD_SNAPSHOT_SCENARIO", scenario)
            .env("OMNI_MD_SNAPSHOT_ROOT", &root)
            .output()
            .expect("run snapshot process scenario");
        assert!(
            output.status.success(),
            "{scenario}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(feature = "native-markdown-candidate")]
    {
        // The native document API is feature-gated; its retained mapping receives the same process isolation.
        let root = directory.path().join("document");
        fs::create_dir(&root).unwrap();
        let output = snapshot_test_command("immutable_snapshot_process_scenario")
            .env("OMNI_MD_SNAPSHOT_SCENARIO", "document")
            .env("OMNI_MD_SNAPSHOT_ROOT", &root)
            .output()
            .expect("run native document snapshot scenario");
        assert!(
            output.status.success(),
            "document: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// Build a single-test subprocess invocation without invoking a shell or interpolating source paths.
fn snapshot_test_command(name: &str) -> std::process::Command {
    // The test harness owns the executable path and exact test selector; all variable data stays in env values.
    let mut command = std::process::Command::new(std::env::current_exe().expect("test executable"));
    command.args([
        "--exact",
        &format!("markdown_workspace::tests::{name}"),
        "--nocapture",
        "--test-threads=1",
    ]);
    command
}

/// Change a source inode from a different process after the reader has captured its immutable snapshot.
fn mutate_snapshot_source(path: &std::path::Path, mode: &str) {
    // Capturing output makes any writer failure visible without mixing nested test-harness output into the suite.
    let output = snapshot_test_command("immutable_snapshot_external_writer")
        .env("OMNI_MD_SNAPSHOT_MUTATION", mode)
        .env("OMNI_MD_SNAPSHOT_SOURCE", path)
        .output()
        .expect("external snapshot writer");
    assert!(
        output.status.success(),
        "external writer: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Count actual open descriptors only inside a single-test child with no competing test threads.
fn snapshot_descriptor_count() -> usize {
    // The directory iterator itself contributes equally to before/after counts and drops after this expression.
    fs::read_dir("/dev/fd")
        .expect("descriptor directory")
        .count()
}

/// Verify immutable borrowing, search/recovery races, and real descriptor/VM release within one isolated child.
#[test]
fn immutable_snapshot_process_scenario() {
    let Ok(scenario) = std::env::var("OMNI_MD_SNAPSHOT_SCENARIO") else {
        // Ordinary suite execution does not run a child scenario without its explicit parent-owned fixture.
        return;
    };
    let root = PathBuf::from(std::env::var_os("OMNI_MD_SNAPSHOT_ROOT").expect("scenario root"));
    let path = root.join("Note.md");
    let original = "stable 中文😀 target\r\n".repeat(8_000);
    fs::write(&path, &original).unwrap();

    match scenario.as_str() {
        // Borrowed UTF-8 must stay immutable even when the source is truncated during the inspection callback.
        "inspection" => {
            let outcome = super::inspect_bounded_markdown_text_cancellable(
                &path,
                super::MAX_DOCUMENT_BYTES,
                || true,
                |text| {
                    // Each visit reads the same frozen source after actual external mutations.
                    // No whole-document slice or second owned text is needed to prove immutable bytes.
                    for mutation in ["truncate", "nonutf8"] {
                        mutate_snapshot_source(&path, mutation);
                        text.visit_validated_bytes(&|| true, |offset, bytes| {
                            assert_eq!(bytes, &original.as_bytes()[offset..offset + bytes.len()]);
                            Ok(())
                        })?;
                    }
                    Ok(())
                },
                None,
            );
            assert!(matches!(
                outcome,
                Err(MarkdownWorkspaceError::DocumentChangedDuringRead)
            ));
        }
        // Concurrently changing note bytes may produce an explicit read/encoding failure, never invalid borrowed text.
        "search" => {
            let workspace = MarkdownWorkspace::new(root.clone(), root.join("app-data"));
            let access = workspace.open_session().unwrap();
            let payload = root.join("valid.bin");
            fs::write(&payload, &original).unwrap();
            let writer = start_snapshot_stress_writer(&path, &payload);
            for _ in 0..120 {
                // Each search owns only transient immutable snapshots; errors represent observed source changes.
                match access.search_notes("target", 5) {
                    // Every returned snippet has already passed immutable UTF-8 validation.
                    Ok(results) => assert!(
                        results
                            .iter()
                            .all(|result| result.relative_path == "Note.md")
                    ),
                    // Active external mutations can invalidate the observed source extent or encoding.
                    Err(
                        MarkdownWorkspaceError::DocumentChangedDuringRead
                        | MarkdownWorkspaceError::InvalidUtf8Document,
                    ) => {}
                    // Other errors indicate a regression in snapshot acquisition or search ownership.
                    Err(error) => panic!("unexpected changing-source search error: {error:?}"),
                }
            }
            assert_snapshot_writer_finished(writer);
            assert_eq!(access.search_notes("target", 5).unwrap().len(), 1);
            workspace.close_session();
        }
        // Owner-only recovery permissions do not prevent another same-user process from modifying the inode.
        #[cfg(feature = "native-markdown-candidate")]
        "recovery" => {
            let workspace = MarkdownWorkspace::new(root.clone(), root.join("app-data"));
            let access = workspace.open_session().unwrap();
            let draft = "unsaved recovery 中文😀\n".repeat(4_000);
            access.write_conflict_fixture("Note.md", &draft).unwrap();
            let record =
                conflict_draft_path(&root.join("app-data/markdown-conflict-recovery"), "Note.md");
            let payload = root.join("valid.bin");
            fs::write(&payload, fs::read(&record).unwrap()).unwrap();
            let writer = start_snapshot_stress_writer(&record, &payload);
            for _ in 0..120 {
                // Recovery may reject a corrupt or changing record; it must never silently substitute disk text.
                match access.open_document_buffer("Note.md") {
                    // A recovered record must be one complete valid payload rather than mixed mutable mapping bytes.
                    Ok(document) => {
                        assert!(document.has_pending_legacy_recovery());
                        let mut bytes = Vec::new();
                        document.write_to(&mut bytes).unwrap();
                        assert_eq!(bytes, draft.as_bytes());
                    }
                    // Truncation, invalid magic/UTF-8, and identity mutation are explicit recovery failures.
                    Err(MarkdownWorkspaceError::ConflictDraftCorrupted) => {}
                    // Unrelated failures expose a broken recovery or snapshot boundary.
                    Err(error) => panic!("unexpected changing recovery error: {error:?}"),
                }
            }
            assert_snapshot_writer_finished(writer);
            let document = access.open_document_buffer("Note.md").unwrap();
            let mut bytes = Vec::new();
            document.write_to(&mut bytes).unwrap();
            assert_eq!(bytes, draft.as_bytes());
            workspace.close_session();
        }
        // An immutable map must retain neither descriptors nor a named scratch tree, then release its VM region on Drop.
        "release" => {
            let source = fs::File::open(&path).unwrap();
            let baseline = snapshot_descriptor_count();
            let mapping =
                crate::immutable_snapshot::snapshot_file(&source, original.len() as u64, || {
                    // This deterministic release scenario has no foreground cancellation source.
                    true
                })
                .unwrap()
                .unwrap();
            assert_eq!(snapshot_descriptor_count(), baseline);
            #[cfg(target_os = "macos")]
            {
                // Query the VM map rather than mincore: Darwin may report residency for an unmapped address.
                let address = mapping.as_ptr() as u64;
                assert!(snapshot_address_is_mapped(address));
                drop(mapping);
                assert!(!snapshot_address_is_mapped(address));
            }
            #[cfg(not(target_os = "macos"))]
            {
                // Portable tests still verify descriptor release; native VM-region evidence is recorded on macOS.
                drop(mapping);
            }
            assert_eq!(snapshot_descriptor_count(), baseline);
        }
        #[cfg(feature = "native-markdown-candidate")]
        // A live piece tree must retain original and inserted bytes after independent source-inode mutations.
        "document" => {
            let workspace = MarkdownWorkspace::new(root.clone(), root.join("app-data"));
            let access = workspace.open_session().unwrap();
            let baseline = snapshot_descriptor_count();
            let mut document = access.open_document_buffer("Note.md").unwrap();
            document
                .insert_utf16(0, "draft ", MarkdownTextSelection::caret(6))
                .unwrap();
            let expected = format!("draft {original}");
            mutate_snapshot_source(&path, "truncate");
            let mut streamed = Vec::new();
            document.write_to(&mut streamed).unwrap();
            assert_eq!(streamed, expected.as_bytes());
            assert_eq!(
                access.save_document_buffer(&mut document),
                Err(MarkdownWorkspaceError::SaveConflict)
            );
            mutate_snapshot_source(&path, "nonutf8");
            streamed.clear();
            document.write_to(&mut streamed).unwrap();
            assert_eq!(streamed, expected.as_bytes());
            assert!(
                document
                    .find_utf16("中文😀", 0, false, true)
                    .unwrap()
                    .is_some()
            );
            assert_eq!(document.word_count(), expected.split_whitespace().count());
            assert!(document.can_undo());
            assert_eq!(
                access.save_document_buffer(&mut document),
                Err(MarkdownWorkspaceError::SaveConflict)
            );
            drop(document);
            workspace.close_session();
            assert_eq!(snapshot_descriptor_count(), baseline);
        }
        // A misspelled scenario would otherwise turn a parent subprocess check into a false success.
        _ => panic!("unsupported snapshot scenario: {scenario}"),
    }
}

/// Start a bounded external writer and wait until it has entered its mutation sequence.
fn start_snapshot_stress_writer(
    path: &std::path::Path,
    payload: &std::path::Path,
) -> std::process::Child {
    // A per-scenario readiness path avoids timing assumptions about subprocess startup and harness initialization.
    let ready = payload.with_extension("ready");
    let child = snapshot_test_command("immutable_snapshot_external_writer")
        .env("OMNI_MD_SNAPSHOT_MUTATION", "stress")
        .env("OMNI_MD_SNAPSHOT_SOURCE", path)
        .env("OMNI_MD_SNAPSHOT_PAYLOAD", payload)
        .env("OMNI_MD_SNAPSHOT_READY", &ready)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn external mutator");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready.exists() {
        // Bound handshake waiting; a missing readiness marker is a failed writer setup, not an implicit pass.
        assert!(
            Instant::now() < deadline,
            "external mutation writer did not start"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    child
}

/// Reap the external writer before accepting final bytes or releasing the test fixture.
fn assert_snapshot_writer_finished(child: std::process::Child) {
    // Waiting consumes child ownership, closes pipes, and prevents a test-owned writer from escaping teardown.
    let output = child.wait_with_output().expect("wait for external mutator");
    assert!(
        output.status.success(),
        "external mutator: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The parent-selected writer runs in a separate process and touches only its explicit fixture path.
#[test]
fn immutable_snapshot_external_writer() {
    let Ok(mode) = std::env::var("OMNI_MD_SNAPSHOT_MUTATION") else {
        // Normal suite execution has no mutation authorization or target path for this helper.
        return;
    };
    let path = PathBuf::from(std::env::var_os("OMNI_MD_SNAPSHOT_SOURCE").expect("mutation source"));
    match mode.as_str() {
        // Truncate the exact inode through a normal external-editor-style write operation.
        "truncate" => fs::write(path, []).unwrap(),
        // Same-process immutable readers must remain safe when the external inode contains invalid UTF-8.
        "nonutf8" => fs::write(path, vec![0xff; 96 * 1_024]).unwrap(),
        // Alternate valid, empty, and invalid bytes under a bounded independent writer lifetime.
        "stress" => {
            let payload =
                fs::read(std::env::var_os("OMNI_MD_SNAPSHOT_PAYLOAD").expect("valid payload"))
                    .unwrap();
            fs::write(
                std::env::var_os("OMNI_MD_SNAPSHOT_READY").expect("ready path"),
                [],
            )
            .unwrap();
            for _ in 0..120 {
                // Each phase exercises a distinct source extent or encoding transition during concurrent reads.
                fs::write(&path, []).unwrap();
                std::thread::sleep(Duration::from_micros(200));
                fs::write(&path, vec![0xff; payload.len()]).unwrap();
                std::thread::sleep(Duration::from_micros(200));
                fs::write(&path, &payload).unwrap();
                std::thread::sleep(Duration::from_micros(200));
            }
            // Leave a complete valid final payload so the parent can verify ordinary reads still recover.
            fs::write(&path, &payload).unwrap();
        }
        // Unknown modes cannot silently satisfy a parent subprocess success assertion.
        _ => panic!("unsupported mutation mode: {mode}"),
    }
}

/// Query the actual task VM region containing a numeric address without reading freed mapping bytes.
#[cfg(target_os = "macos")]
fn snapshot_address_is_mapped(target: u64) -> bool {
    // These stable Darwin signatures match mach/mach_vm.h. The basic-info flavor is 9 and its
    // packed SDK layout occupies nine natural_t words; only the returned address and size are read.
    unsafe extern "C" {
        static mut mach_task_self_: libc::mach_port_t;
        fn mach_vm_region(
            task: libc::mach_port_t,
            address: *mut u64,
            size: *mut u64,
            flavor: i32,
            info: *mut i32,
            count: *mut u32,
            object_name: *mut libc::mach_port_t,
        ) -> libc::kern_return_t;
        fn mach_port_deallocate(
            task: libc::mach_port_t,
            name: libc::mach_port_t,
        ) -> libc::kern_return_t;
    }
    let mut address = target;
    let mut size = 0_u64;
    let mut info = [0_i32; 9];
    let mut count = info.len() as u32;
    let mut object_name = 0;
    // SAFETY: Darwin initializes mach_task_self_ for this process before tests run. Reading its value
    // borrows the current task send right without forming a Rust reference or consuming that right.
    let task = unsafe { mach_task_self_ };
    // SAFETY: every output pointer addresses correctly typed, aligned, writable local storage for the
    // complete SDK basic-info record. The target is a numeric VM query address and is never dereferenced.
    // The task right remains valid, and the synchronous call cannot retain these stack pointers.
    let status = unsafe {
        mach_vm_region(
            task,
            &mut address,
            &mut size,
            9,
            info.as_mut_ptr(),
            &mut count,
            &mut object_name,
        )
    };
    if object_name != 0 {
        // The API may return an owned object send right; release it before returning from the query.
        // SAFETY: a nonzero returned object_name is the send right transferred by this successful query.
        let released = unsafe { mach_port_deallocate(task, object_name) };
        assert_eq!(released, libc::KERN_SUCCESS);
    }
    match status {
        // mach_vm_region returns the containing region or the next one; only actual overlap means mapped.
        libc::KERN_SUCCESS => address <= target && target < address.saturating_add(size),
        // No region remains at or after this address, so the released range is unmapped.
        libc::KERN_INVALID_ADDRESS => false,
        // A failed instrumentation query must fail the test instead of being mistaken for successful release.
        other => panic!("VM region query failed: {other}"),
    }
}
