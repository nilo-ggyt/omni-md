//! Provisional import acceptance and external-mutation-safe rollback over synthetic confined files.

use super::*;
use std::os::unix::fs::symlink;

/// Own disposable source/vault directories and one active metadata-only session for each case.
struct Fixture {
    root: tempfile::TempDir,
    external: tempfile::TempDir,
    access: MarkdownSessionAccess,
}

impl Fixture {
    /// Establish a note and external asset without accessing actual user documents or attachments.
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("Note.md"), "note").unwrap();
        std::fs::write(external.path().join("asset.png"), "synthetic asset").unwrap();
        let access = MarkdownWorkspace::with_root(root.path().to_owned())
            .open_session()
            .unwrap();
        Self {
            root,
            external,
            access,
        }
    }

    /// Acquire a unique operation token for the ordinary synthetic asset.
    fn prepare(&self) -> MarkdownPendingAttachmentImport {
        // Each prepare exclusively chooses its own filename; later cases cannot fabricate a token.
        self.access
            .prepare_attachment_import("Note.md", &self.external.path().join("asset.png"))
            .unwrap()
    }
}

/// Commit persists the acquired bytes, whereas explicit rollback and ordinary Drop remove only their assets.
#[test]
fn provisional_commit_rollback_and_drop_have_distinct_file_ownership() {
    // Accept the first exclusively acquired asset as ordinary vault data.
    let fixture = Fixture::new();
    let committed = fixture.prepare().commit();
    assert_eq!(
        std::fs::read(fixture.root.path().join(committed.attachment_relative_path)).unwrap(),
        b"synthetic asset"
    );
    // A collision receives its own suffix and cannot remove the earlier committed asset.
    let pending = fixture.prepare();
    let second = fixture
        .root
        .path()
        .join(&pending.receipt().attachment_relative_path);
    assert!(second.is_file());
    pending.rollback().unwrap();
    assert!(!second.exists());
    // An uncommitted token's destructor has the same finite ownership scope as explicit rollback.
    let pending = fixture.prepare();
    let dropped = fixture
        .root
        .path()
        .join(&pending.receipt().attachment_relative_path);
    drop(pending);
    assert!(!dropped.exists());
    assert_eq!(
        std::fs::read_dir(fixture.root.path().join("attachments"))
            .unwrap()
            .count(),
        1
    );
}

/// A later writer revokes rollback authority even while the original inode remains open in the token.
#[test]
fn changed_asset_is_preserved_and_explicit_cleanup_reports_conflict() {
    // A cooperating external writer changes the inode after acquisition, revoking our deletion authority.
    let fixture = Fixture::new();
    let pending = fixture.prepare();
    let asset = fixture
        .root
        .path()
        .join(&pending.receipt().attachment_relative_path);
    std::fs::write(&asset, "external changed bytes").unwrap();
    assert_eq!(
        pending.rollback(),
        Err(MarkdownWorkspaceError::DocumentChangedDuringRead)
    );
    assert_eq!(
        std::fs::read_to_string(asset).unwrap(),
        "external changed bytes"
    );
}

/// Replacing the published name cannot redirect rollback to either the new inode or a symlink target.
#[test]
fn replaced_inode_and_symlink_targets_are_never_removed() {
    // Preserve both the renamed original inode and the unrelated replacement at its former name.
    let fixture = Fixture::new();
    let pending = fixture.prepare();
    let asset = fixture
        .root
        .path()
        .join(&pending.receipt().attachment_relative_path);
    let moved = fixture.root.path().join("attachments/moved.png");
    std::fs::rename(&asset, &moved).unwrap();
    std::fs::write(&asset, "external replacement").unwrap();
    assert!(pending.rollback().is_err());
    assert_eq!(
        std::fs::read_to_string(&asset).unwrap(),
        "external replacement"
    );
    assert_eq!(std::fs::read_to_string(moved).unwrap(), "synthetic asset");

    // A separate token is replaced with an outbound symlink after acquisition.
    let pending = fixture.prepare();
    let path = fixture
        .root
        .path()
        .join(&pending.receipt().attachment_relative_path);
    std::fs::remove_file(&path).unwrap();
    let victim = fixture.external.path().join("victim.png");
    std::fs::write(&victim, "preserve victim").unwrap();
    symlink(&victim, &path).unwrap();
    assert!(pending.rollback().is_err());
    assert!(
        std::fs::symlink_metadata(path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read_to_string(victim).unwrap(), "preserve victim");
}

/// A newly introduced hardlink preserves both names rather than treating shared data as disposable.
#[test]
fn late_hardlink_revokes_exclusive_cleanup() {
    // A second name makes the acquired bytes shared data instead of a disposable single-link asset.
    let fixture = Fixture::new();
    let pending = fixture.prepare();
    let asset = fixture
        .root
        .path()
        .join(&pending.receipt().attachment_relative_path);
    let alias = fixture.external.path().join("alias.png");
    std::fs::hard_link(&asset, &alias).unwrap();
    assert!(pending.rollback().is_err());
    assert_eq!(std::fs::read(&asset).unwrap(), b"synthetic asset");
    assert_eq!(std::fs::read(alias).unwrap(), b"synthetic asset");
}

/// Parent directory replacement or redirection preserves all entries in both old and new directories.
#[test]
fn replaced_parent_and_outbound_directory_symlink_cannot_redirect_cleanup() {
    // Moving the pinned directory away must not authorize cleanup in the newly published parent.
    let fixture = Fixture::new();
    let pending = fixture.prepare();
    let name = Path::new(&pending.receipt().attachment_relative_path)
        .file_name()
        .unwrap()
        .to_owned();
    let attachments = fixture.root.path().join("attachments");
    let moved = fixture.root.path().join("Held");
    std::fs::rename(&attachments, &moved).unwrap();
    std::fs::create_dir(&attachments).unwrap();
    std::fs::write(attachments.join(&name), "new directory victim").unwrap();
    assert!(pending.rollback().is_err());
    assert_eq!(
        std::fs::read_to_string(attachments.join(&name)).unwrap(),
        "new directory victim"
    );
    assert_eq!(
        std::fs::read(moved.join(&name)).unwrap(),
        b"synthetic asset"
    );

    // Redirect a newly prepared token's parent outside the vault; neither directory may be cleaned.
    let pending = fixture.prepare();
    let name = Path::new(&pending.receipt().attachment_relative_path)
        .file_name()
        .unwrap()
        .to_owned();
    let held = fixture.root.path().join("HeldAgain");
    std::fs::rename(&attachments, &held).unwrap();
    std::fs::write(
        fixture.external.path().join(&name),
        "outside directory victim",
    )
    .unwrap();
    symlink(fixture.external.path(), &attachments).unwrap();
    assert!(pending.rollback().is_err());
    assert_eq!(
        std::fs::read_to_string(fixture.external.path().join(&name)).unwrap(),
        "outside directory victim"
    );
    assert_eq!(std::fs::read(held.join(&name)).unwrap(), b"synthetic asset");
}

/// Actual canonical insertion rejection can roll back the provisional file without changing the note.
#[cfg(feature = "native-markdown-candidate")]
#[test]
fn rejected_canonical_link_insertion_leaves_no_orphan_asset_or_document_edit() {
    use crate::{MarkdownDocumentBuffer, MarkdownDocumentBufferError, MarkdownTextSelection};
    // Acquire real asset bytes before forcing the actual document's canonical insertion budget to reject.
    let fixture = Fixture::new();
    let mut document =
        MarkdownDocumentBuffer::open(&fixture.root.path().join("Note.md"), 4).unwrap();
    let pending = fixture.prepare();
    let asset = fixture
        .root
        .path()
        .join(&pending.receipt().attachment_relative_path);
    let before = document.storage_profile();
    let insertion = &pending.receipt().markdown_insertion;
    let selection = MarkdownTextSelection::caret(insertion.encode_utf16().count());
    assert_eq!(
        document.replace_utf16_range_with_selection(
            0..0,
            insertion,
            MarkdownTextSelection::caret(0),
            selection
        ),
        Err(MarkdownDocumentBufferError::DocumentTooLarge)
    );
    // The failed text edit leaves authority in the token, so explicit rollback removes only its new asset.
    pending.rollback().unwrap();
    assert!(!asset.exists());
    assert_eq!(document.storage_profile(), before);
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("Note.md")).unwrap(),
        "note"
    );
}

/// An observed source shrink must clean up a partially copied new inode without exposing a receipt.
#[test]
fn failed_partial_copy_removes_its_owned_output() {
    let fixture = Fixture::new();
    // Internal acquisition receives the same canonical authority as the public session entry point.
    let canonical_root = fixture.root.path().canonicalize().unwrap();
    let directory = ensure_attachments_directory(&canonical_root).unwrap();
    let mut source = File::open(fixture.external.path().join("asset.png")).unwrap();
    // Ask the real finite copy loop for more bytes than exist, causing EOF after it has written a prefix.
    let result = create_collision_safe_attachment(
        &canonical_root,
        &directory,
        "partial.png",
        &mut source,
        4_096,
    );
    assert!(matches!(
        result,
        Err(MarkdownWorkspaceError::DocumentChangedDuringRead)
    ));
    // Failure seals and drops the acquired inode; no provisional receipt or orphan asset survives.
    assert_eq!(std::fs::read_dir(directory).unwrap().count(), 0);
    assert_eq!(
        std::fs::read(fixture.root.path().join("Note.md")).unwrap(),
        b"note"
    );
    assert_eq!(
        std::fs::read(fixture.external.path().join("asset.png")).unwrap(),
        b"synthetic asset"
    );
}

/// An already redirected parent is rejected before acquiring an asset inode outside the vault.
#[test]
fn out_of_vault_parent_is_rejected_before_asset_creation() {
    let fixture = Fixture::new();
    let root = fixture.root.path().canonicalize().unwrap();
    let outside = fixture.external.path().canonicalize().unwrap();
    let mut source = File::open(outside.join("asset.png")).unwrap();
    // Exercise internal acquisition with a stale/outbound parent authority, as a raced caller might observe.
    let result = create_collision_safe_attachment(&root, &outside, "outbound.png", &mut source, 15);
    assert!(matches!(
        result,
        Err(MarkdownWorkspaceError::PathEscapesRoot)
    ));
    // No copied prefix or provisional name may appear before confinement validation has succeeded.
    assert!(!outside.join("outbound.png").exists());
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
    assert_eq!(
        std::fs::read(outside.join("asset.png")).unwrap(),
        b"synthetic asset"
    );
}
