//! Fail after actual synthetic filesystem changes and check both prior authority
//! and authority acquired while the same operation was still in progress.

use super::super::*;
use super::*;
use std::collections::HashSet;
use std::sync::atomic::AtomicBool;

/// Acquire real public directory/body-search pages, including independently
/// revocable terminal-page authority rather than a fabricated publication count.
fn authorities(
    session: &MarkdownSessionAccess,
) -> (MarkdownWorkspacePageAuthority, MarkdownSearchAuthority) {
    let mut pager = session.open_workspace_pager().unwrap();
    let MarkdownWorkspacePageProgress::Ready(page) = pager
        .next_page_cancellable(12, 512, &HashSet::new(), &AtomicBool::new(false))
        .unwrap()
    else {
        panic!("小型合成目录应一步完整扫描");
    };
    let search = session
        .search_notes_page_cancellable("before", 13, None, &AtomicBool::new(false))
        .unwrap();
    (page.authority, search.authority)
}

/// Check both authority domains; search invalidation alone could conceal a
/// missing directory-page revocation.
fn assert_revoked(
    authority: &(MarkdownWorkspacePageAuthority, MarkdownSearchAuthority),
    session: &MarkdownSessionAccess,
) {
    assert!(!authority.0.is_current(session, &HashSet::new()));
    assert!(!authority.1.is_current(session, "before"));
}

#[cfg(feature = "native-markdown-candidate")]
#[test]
/// Fail durability after each actual structural effect; neither an old page nor
/// a page acquired mid-operation may survive the final outcome.
fn structure_sync_failures_revoke_old_and_mid_operation_authorities() {
    for operation in [
        "create_note",
        "create_directory",
        "rename_directory",
        "delete_directory",
        "import",
        "rename_note",
        "move_plain",
        "move_rewritten",
        "copy",
        "delete_note",
        "save_as_new",
    ] {
        // Independent real fixtures prevent earlier revocation or retained files
        // from hiding a missing invalidation in the current operation.
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("sub/child.md"), "before").unwrap();
        let content = if operation == "move_rewritten" {
            // A depth-changing attachment link selects real rewritten-destination
            // creation followed by source removal rather than the plain rename path.
            "before [asset](attachments/pic.png)"
        } else {
            // The plain move keeps body bytes unchanged and uses exclusive rename.
            "before"
        };
        fs::write(root.join("a.md"), content).unwrap();
        fs::write(root.join("b.md"), "before").unwrap();
        let external = fixture.path().join("External.md");
        fs::write(&external, "before external").unwrap();
        let workspace = MarkdownWorkspace::new(root.clone());
        let session = workspace.open_session().unwrap();
        let old = authorities(&session);
        let mut mid = None;
        let mut called = 0;
        let mut fail_sync = |_: &Path| {
            called += 1;
            // This boundary follows real publication. Check the actual visible
            // names before authority; a successful operation return is not assumed.
            match operation {
                "create_note" => assert!(root.join("New.md").is_file()),
                "create_directory" => assert!(root.join("NewDir").is_dir()),
                "rename_directory" => {
                    assert!(!root.join("sub").exists());
                    assert!(root.join("RenamedDir").is_dir());
                }
                "delete_directory" => assert!(!root.join("sub").exists()),
                "import" => assert!(root.join("External.md").is_file()),
                "rename_note" => {
                    assert!(!root.join("a.md").exists());
                    assert!(root.join("Renamed.md").is_file());
                }
                "move_plain" => {
                    assert!(!root.join("a.md").exists());
                    assert!(root.join("sub/a.md").is_file());
                }
                "move_rewritten" => {
                    assert!(root.join("a.md").is_file());
                    assert!(root.join("sub/a.md").is_file());
                }
                "copy" => assert!(root.join("a 副本.md").is_file()),
                "delete_note" => assert!(!root.join("a.md").exists()),
                "save_as_new" => assert!(root.join("NewCopy.md").is_file()),
                _ => unreachable!("操作表固定"),
            }
            assert_revoked(&old, &session);
            // Acquire actual public page authority after the first effect. Final
            // cleanup must also reject this observed temporary/partial collection.
            mid = Some(authorities(&session));
            assert!(
                mid.as_ref()
                    .unwrap()
                    .0
                    .is_current(&session, &HashSet::new())
            );
            Err(MarkdownWorkspaceError::StorageUnavailable)
        };
        let result = match operation {
            "create_note" => session
                .create_note_with_directory_sync("", "New", &mut fail_sync)
                .map(|_| ()),
            "create_directory" => session
                .create_directory_with_directory_sync("", "NewDir", &mut fail_sync)
                .map(|_| ()),
            "rename_directory" => session
                .rename_directory_with_directory_sync("sub", "RenamedDir", &mut fail_sync)
                .map(|_| ()),
            "delete_directory" => {
                session.delete_directory_with_directory_sync("sub", &mut fail_sync)
            }
            "import" => session
                .import_note_with_directory_sync(&external, "", &mut fail_sync)
                .map(|_| ()),
            "rename_note" => session
                .rename_note_with_directory_sync("a.md", "Renamed", None, &mut fail_sync)
                .map(|_| ()),
            "move_plain" | "move_rewritten" => session
                .move_note_with_directory_sync("a.md", "sub", None, &mut fail_sync)
                .map(|_| ()),
            "copy" => session
                .copy_note_with_directory_sync("a.md", None, &mut fail_sync)
                .map(|_| ()),
            "delete_note" => session.delete_note_with_directory_sync("a.md", None, &mut fail_sync),
            "save_as_new" => {
                // The save-as buffer still belongs to a.md. Failure may clean
                // only its new destination, never alter original characters/history.
                let document = session.open_document_buffer("a.md").unwrap();
                session
                    .save_document_buffer_as_new_with_directory_sync(
                        &document,
                        "",
                        "NewCopy",
                        &mut fail_sync,
                    )
                    .map(|_| ())
            }
            _ => unreachable!("操作表固定"),
        };
        assert_eq!(
            result,
            Err(MarkdownWorkspaceError::StorageUnavailable),
            "{operation}"
        );
        assert_eq!(called, 1, "{operation}");
        assert_revoked(&old, &session);
        assert_revoked(mid.as_ref().unwrap(), &session);
        if matches!(
            operation,
            "import" | "copy" | "move_rewritten" | "save_as_new"
        ) {
            // These operations retain destination rollback. Final revocation
            // must follow the guard's actual removal of its candidate.
            let destination = match operation {
                "import" => "External.md",
                "copy" => "a 副本.md",
                "move_rewritten" => "sub/a.md",
                _ => "NewCopy.md",
            };
            assert!(!root.join(destination).exists(), "{operation}");
            assert!(root.join("a.md").is_file());
        }
    }
}

#[test]
/// A failed partial write after exclusive open retains bytes whose final identity
/// cannot be proved and revokes authority acquired during that write.
fn created_note_write_failure_and_rollback_revoke_both_generations() {
    let fixture = tempfile::tempdir().unwrap();
    fs::write(fixture.path().join("a.md"), "before").unwrap();
    let workspace = MarkdownWorkspace::new(fixture.path().to_owned());
    let session = workspace.open_session().unwrap();
    let old = authorities(&session);
    let publication = WorkspaceStructurePublication::new(&session);
    let created = fixture.path().join("Partial.md");
    let mut mid = None;
    let result = create_new_markdown_file_with_writer(
        &publication,
        &created,
        b"complete",
        None,
        |file, _| {
            // Exclusive creation already revoked old pages. Write an actual
            // prefix before failing, so failure follows a real visible side effect.
            assert!(created.is_file());
            assert_revoked(&old, &session);
            file.write_all(b"partial before")?;
            mid = Some(authorities(&session));
            Err(std::io::Error::other("synthetic post-create write failure"))
        },
    );
    assert!(result.is_err());
    assert_eq!(fs::read(&created).unwrap(), b"partial before");
    let result = publication.finish(
        result
            .map(|_| ())
            .map_err(|_| MarkdownWorkspaceError::StorageUnavailable),
    );
    assert!(
        matches!(result, Err(MarkdownWorkspaceError::RollbackUnconfirmed { cause }) if *cause == MarkdownWorkspaceError::StorageUnavailable)
    );
    drop(publication);
    assert_revoked(mid.as_ref().unwrap(), &session);
    assert_revoked(&old, &session);
}

#[test]
/// The existing private removal boundary deletes a real child before failing;
/// the surviving directory cannot keep authority acquired after that partial change.
fn partial_recursive_delete_error_is_conservatively_revoked_without_reimplementing_std() {
    let fixture = tempfile::tempdir().unwrap();
    fs::create_dir(fixture.path().join("sub")).unwrap();
    fs::write(fixture.path().join("a.md"), "before").unwrap();
    fs::write(fixture.path().join("sub/removed.md"), "before").unwrap();
    fs::write(fixture.path().join("sub/kept.md"), "before").unwrap();
    let workspace = MarkdownWorkspace::new(fixture.path().to_owned());
    let session = workspace.open_session().unwrap();
    let old = authorities(&session);
    let mut mid = None;
    let result = session.delete_directory_with_removal(
        "sub",
        |_| panic!("失败移除不能进入sync"),
        |path| {
            // Revocation precedes opaque recursive removal; now perform and
            // observe a real partial deletion before returning the failure.
            assert_revoked(&old, &session);
            fs::remove_file(path.join("removed.md"))?;
            assert!(!path.join("removed.md").exists());
            assert!(path.join("kept.md").is_file());
            mid = Some(authorities(&session));
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        },
    );
    assert_eq!(result, Err(MarkdownWorkspaceError::StorageUnavailable));
    assert!(fixture.path().join("sub/kept.md").exists());
    assert!(!fixture.path().join("sub/removed.md").exists());
    assert_revoked(mid.as_ref().unwrap(), &session);
}

#[test]
/// Failed source revalidation drops the returned candidate guard; actual rollback
/// and final publication must both revoke pages acquired along the way.
fn source_revalidation_failure_releases_provisional_note_and_revokes_mid_page() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("vault");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("a.md"), "before").unwrap();
    let source = fixture.path().join("source.md");
    fs::write(&source, "before source").unwrap();
    let workspace = MarkdownWorkspace::new(root.clone());
    let session = workspace.open_session().unwrap();
    let publication = WorkspaceStructurePublication::new(&session);
    let mut mid = None;
    let result = inspect_bounded_markdown(&source, MAX_DOCUMENT_BYTES, |content| {
        // The provisional target streams the original frozen windows and retains
        // its actual rollback owner until source/path revalidation has completed.
        let pending = create_collision_safe_imported_note_with_writer(
            &publication,
            &root,
            "Imported",
            content.len() as u64,
            |file| content.copy_to(file).map_err(std::io::Error::other),
        )?;
        assert!(pending.path.is_file());
        mid = Some(authorities(&session));
        // Change the real source extent to fail post-read identity validation
        // after an actual provisional destination has already been created.
        fs::write(&source, "changed source with a different byte count").unwrap();
        Ok(pending)
    });
    assert!(matches!(
        result,
        Err(MarkdownWorkspaceError::DocumentChangedDuringRead)
    ));
    assert!(!root.join("Imported.md").exists());
    assert_revoked(mid.as_ref().unwrap(), &session);
    drop(result);
    drop(publication);
}

#[test]
/// Root protection before deletion and real no-ops leave publication inactive,
/// preserving otherwise-current pages without unnecessary revocation.
fn preflight_rejection_and_noop_leave_authority_current() {
    let fixture = tempfile::tempdir().unwrap();
    fs::create_dir(fixture.path().join("sub")).unwrap();
    fs::write(fixture.path().join("a.md"), "before").unwrap();
    let workspace = MarkdownWorkspace::new(fixture.path().to_owned());
    let session = workspace.open_session().unwrap();
    let old = authorities(&session);
    let result = session.delete_directory_with_removal(
        "",
        |_| panic!("root保护不sync"),
        |_| panic!("root保护不进入删除"),
    );
    assert_eq!(result, Err(MarkdownWorkspaceError::RootDirectoryProtected));
    session.rename_directory("sub", "sub").unwrap();
    session.rename_note("a.md", "a", None).unwrap();
    session.move_note("a.md", "", None).unwrap();
    assert!(old.0.is_current(&session, &HashSet::new()));
    assert!(old.1.is_current(&session, "before"));
}

#[cfg(feature = "native-markdown-candidate")]
#[test]
/// After replacement or in-place mutation of complete candidates, all four
/// rollback paths preserve external bytes and the original operation failure.
fn provisional_note_rollback_preserves_replacements_and_same_inode_modifications() {
    for operation in ["import", "copy", "move_rewritten", "save_as_new"] {
        for replacement in [true, false] {
            // Reacquire real files and authority per case; state retained by an
            // earlier failure cannot alter naming or bypass the next cleanup path.
            let fixture = tempfile::tempdir().unwrap();
            let root = fixture.path().join("vault");
            fs::create_dir_all(root.join("sub")).unwrap();
            fs::write(root.join("a.md"), "before [asset](attachments/p.png)").unwrap();
            let source = fixture.path().join("External.md");
            fs::write(&source, "before external").unwrap();
            let workspace = MarkdownWorkspace::new(root.clone());
            let session = workspace.open_session().unwrap();
            let destination = root.join(match operation {
                // Use each operation's actual naming rule; do not construct a
                // guard directly and pretend its production call chain ran.
                "import" => "External.md",
                "copy" => "a 副本.md",
                "move_rewritten" => "sub/a.md",
                "save_as_new" => "NewCopy.md",
                _ => unreachable!("固定操作矩阵"),
            });
            let mut tampered = false;
            let result = {
                let mut fail_sync = |_: &Path| {
                    let acquired_inode = fs::metadata(&destination).unwrap().ino();
                    if replacement {
                        // A real new inode takes the name via rename while the
                        // acquired descriptor still belongs to the removed original.
                        let external = root.join("external-replacement.tmp");
                        fs::write(&external, b"EXTERNAL replacement bytes").unwrap();
                        fs::rename(external, &destination).unwrap();
                        assert_ne!(fs::metadata(&destination).unwrap().ino(), acquired_inode);
                    } else {
                        // Rewrite the original inode with a different extent;
                        // its captured size/ctime/mtime identity must no longer match.
                        fs::write(
                            &destination,
                            b"EXTERNAL modified bytes with distinct length",
                        )
                        .unwrap();
                        assert_eq!(fs::metadata(&destination).unwrap().ino(), acquired_inode);
                    }
                    tampered = true;
                    Err(MarkdownWorkspaceError::StorageUnavailable)
                };
                match operation {
                    // Failure follows complete acquisition/sealing. Drop must
                    // refuse cleanup once that sealed identity has been lost.
                    "import" => session
                        .import_note_with_directory_sync(&source, "", &mut fail_sync)
                        .map(|_| ()),
                    "copy" => session
                        .copy_note_with_directory_sync("a.md", None, &mut fail_sync)
                        .map(|_| ()),
                    "move_rewritten" => session
                        .move_note_with_directory_sync("a.md", "sub", None, &mut fail_sync)
                        .map(|_| ()),
                    "save_as_new" => {
                        let document = session.open_document_buffer("a.md").unwrap();
                        session
                            .save_document_buffer_as_new_with_directory_sync(
                                &document,
                                "",
                                "NewCopy",
                                &mut fail_sync,
                            )
                            .map(|_| ())
                    }
                    _ => unreachable!("固定操作矩阵"),
                }
            };
            assert!(tampered);
            assert!(
                matches!(result, Err(MarkdownWorkspaceError::RollbackUnconfirmed { cause }) if *cause == MarkdownWorkspaceError::StorageUnavailable)
            );
            let expected: &[u8] = if replacement {
                // Owning the former complete bytes grants no right to delete
                // the independent replacement under the same name.
                b"EXTERNAL replacement bytes"
            } else {
                // The same inode alone is insufficient cleanup authority;
                // preserve its externally modified complete bytes too.
                b"EXTERNAL modified bytes with distinct length"
            };
            assert_eq!(
                fs::read(&destination).unwrap(),
                expected,
                "{operation}/{replacement}"
            );
            assert!(root.join("a.md").is_file());
        }
    }
}

#[test]
/// Added hardlinks and symlink replacement revoke cleanup authority; rollback
/// cannot follow a link and remove an unrelated external file.
fn provisional_note_rollback_rejects_added_aliases_and_symlinks() {
    use std::os::unix::fs::symlink;
    for symbolic in [false, true] {
        // Acquire a complete file for this case and keep the external victim
        // under an independent synthetic path with known exact bytes.
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("a.md"), "before").unwrap();
        let workspace = MarkdownWorkspace::new(root.clone());
        let session = workspace.open_session().unwrap();
        let destination = root.join("a 副本.md");
        let external = fixture.path().join("external.md");
        let result = session.copy_note_with_directory_sync("a.md", None, |_| {
            if symbolic {
                // A same-name symlink grants the old acquired FD no authority
                // to unlink either the substituted name or its target.
                fs::write(&external, b"external target").unwrap();
                fs::remove_file(&destination).unwrap();
                symlink(&external, &destination).unwrap();
            } else {
                // A changed link count means the bytes are shared; removing the
                // original name cannot be reported as an exclusively owned rollback.
                fs::hard_link(&destination, &external).unwrap();
            }
            Err(MarkdownWorkspaceError::SaveConflict)
        });
        assert!(
            matches!(result, Err(MarkdownWorkspaceError::RollbackUnconfirmed { cause }) if *cause == MarkdownWorkspaceError::SaveConflict)
        );
        assert!(fs::symlink_metadata(&destination).is_ok());
        assert_eq!(
            fs::read(&external).unwrap(),
            if symbolic {
                b"external target".as_slice()
            } else {
                b"before".as_slice()
            }
        );
    }
}

#[test]
/// An unchanged file identity grants no out-of-vault cleanup authority when its
/// parent moved outside the vault and was reconnected through a symlink.
fn provisional_note_rollback_rejects_moved_parent_reconnected_by_symlink() {
    use std::os::unix::fs::symlink;
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("vault");
    let original_parent = root.join("sub");
    let moved_parent = fixture.path().join("outside");
    fs::create_dir_all(&original_parent).unwrap();
    fs::write(original_parent.join("a.md"), b"before original").unwrap();
    let workspace = MarkdownWorkspace::new(root);
    let session = workspace.open_session().unwrap();
    let result = session.copy_note_with_directory_sync("sub/a.md", None, |_| {
        // Preserve the original file inode/bytes while changing only its real
        // parent confinement, so path containment must independently reject cleanup.
        fs::rename(&original_parent, &moved_parent).unwrap();
        symlink(&moved_parent, &original_parent).unwrap();
        Err(MarkdownWorkspaceError::StorageUnavailable)
    });
    assert!(
        matches!(result, Err(MarkdownWorkspaceError::RollbackUnconfirmed { cause }) if *cause == MarkdownWorkspaceError::StorageUnavailable)
    );
    assert_eq!(
        fs::read(moved_parent.join("a 副本.md")).unwrap(),
        b"before original"
    );
    assert_eq!(
        fs::read(moved_parent.join("a.md")).unwrap(),
        b"before original"
    );
}

#[test]
/// A same-path replacement during a failed partial write remains external data;
/// no complete seal exists that could authorize deleting it.
fn partial_writer_failure_does_not_remove_external_replacement() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("vault");
    fs::create_dir(&root).unwrap();
    let destination = root.join("Partial.md");
    let workspace = MarkdownWorkspace::new(root);
    let session = workspace.open_session().unwrap();
    let publication = WorkspaceStructurePublication::new(&session);
    let result = create_new_markdown_file_with_writer(
        &publication,
        &destination,
        b"complete expected",
        None,
        |file, _| {
            // Write a real partial prefix, replace its name with another inode,
            // then fail the original write stage after those observable effects.
            file.write_all(b"partial")?;
            let external = fixture.path().join("external.tmp");
            fs::write(&external, b"external replacement during failed write")?;
            fs::rename(external, &destination)?;
            Err(std::io::Error::from(std::io::ErrorKind::WriteZero))
        },
    );
    let result = publication.finish(
        result
            .map(|_| ())
            .map_err(|_| MarkdownWorkspaceError::StorageUnavailable),
    );
    assert!(
        matches!(result, Err(MarkdownWorkspaceError::RollbackUnconfirmed { cause }) if *cause == MarkdownWorkspaceError::StorageUnavailable)
    );
    assert_eq!(
        fs::read(&destination).unwrap(),
        b"external replacement during failed write"
    );
}

#[test]
/// Source unlink is a rewritten move's second publication point. Later sync
/// failure preserves its destination while revoking newly acquired intermediate pages.
fn rewritten_move_second_publication_revokes_intermediate_pages_even_when_final_sync_fails() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("vault");
    fs::create_dir_all(root.join("sub")).unwrap();
    fs::write(root.join("a.md"), "before [asset](attachments/p.png)").unwrap();
    let workspace = MarkdownWorkspace::new(root.clone());
    let session = workspace.open_session().unwrap();
    let mut first = None;
    let mut second = None;
    let mut calls = 0;
    let result = session.move_note_with_directory_sync("a.md", "sub", None, |_| {
        calls += 1;
        if calls == 1 {
            // The complete destination exists alongside its source. Acquire real
            // page authority for this first intermediate filesystem collection.
            assert!(root.join("a.md").is_file());
            assert!(root.join("sub/a.md").is_file());
            first = Some(authorities(&session));
            Ok(())
        } else {
            // Source unlink already happened; immediate revocation must precede
            // this second fallible directory-sync boundary.
            assert_eq!(calls, 2);
            assert!(!root.join("a.md").exists());
            assert_revoked(first.as_ref().unwrap(), &session);
            second = Some(authorities(&session));
            Err(MarkdownWorkspaceError::StorageUnavailable)
        }
    });
    assert_eq!(result, Err(MarkdownWorkspaceError::StorageUnavailable));
    assert_eq!(calls, 2);
    assert_revoked(second.as_ref().unwrap(), &session);
    assert!(!root.join("a.md").exists());
    assert_eq!(
        fs::read(root.join("sub/a.md")).unwrap(),
        b"before [asset](../attachments/p.png)"
    );
}

#[test]
/// A rewritten move without a caller revision must still preserve source bytes
/// that an external writer changed during destination durability.
fn rewritten_move_rechecks_its_own_source_revision_without_caller_precondition() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("vault");
    fs::create_dir_all(root.join("sub")).unwrap();
    let source = root.join("a.md");
    fs::write(&source, b"before [asset](attachments/p.png)").unwrap();
    let workspace = MarkdownWorkspace::new(root.clone());
    let session = workspace.open_session().unwrap();
    let mut calls = 0;
    let result = session.move_note_with_directory_sync("a.md", "sub", None, |_| {
        calls += 1;
        // The destination is fully sealed. Actually update the source during
        // the sync boundary, which itself still returns success.
        assert_eq!(calls, 1);
        fs::write(&source, b"EXTERNAL new source bytes").unwrap();
        Ok(())
    });
    assert_eq!(result, Err(MarkdownWorkspaceError::SaveConflict));
    assert_eq!(calls, 1);
    assert_eq!(fs::read(&source).unwrap(), b"EXTERNAL new source bytes");
    assert!(!root.join("sub/a.md").exists());
}

#[test]
/// A successful directory sync proves no destination identity. In-place writes
/// and same-name replacement must preserve both source and external destination.
fn successful_sync_does_not_commit_a_modified_provisional_note() {
    for operation in [
        "import",
        "copy",
        "move_rewritten",
        #[cfg(feature = "native-markdown-candidate")]
        "save_as_new",
    ] {
        for replacement in [false, true] {
            // Independent real fixtures keep one failure's retained files from
            // changing another operation's collision names or revision checks.
            let fixture = tempfile::tempdir().unwrap();
            let root = fixture.path().join("vault");
            fs::create_dir_all(root.join("sub")).unwrap();
            let original = b"before [asset](attachments/p.png)";
            fs::write(root.join("a.md"), original).unwrap();
            let external_source = fixture.path().join("External.md");
            fs::write(&external_source, original).unwrap();
            let workspace = MarkdownWorkspace::new(root.clone());
            let session = workspace.open_session().unwrap();
            let destination = root.join(match operation {
                "import" => "External.md",
                "copy" => "a 副本.md",
                "move_rewritten" => "sub/a.md",
                "save_as_new" => "NewCopy.md",
                _ => unreachable!("固定操作矩阵"),
            });
            let mut calls = 0;
            let mut synchronize = |_: &Path| {
                calls += 1;
                assert_eq!(calls, 1);
                if replacement {
                    // Replace the name with an independent inode; the original
                    // FD still owns the old value and cannot authorize deleting the new one.
                    let temporary = root.join("external.tmp");
                    fs::write(&temporary, b"EXTERNAL destination bytes").unwrap();
                    fs::rename(temporary, &destination).unwrap();
                } else {
                    // In-place body changes also revoke the seal. An unchanged
                    // pathname alone cannot authorize committing externally written bytes.
                    fs::write(&destination, b"EXTERNAL destination bytes").unwrap();
                }
                Ok(())
            };
            let result = match operation {
                "import" => {
                    session.import_note_with_directory_sync(&external_source, "", &mut synchronize)
                }
                "copy" => session.copy_note_with_directory_sync("a.md", None, &mut synchronize),
                "move_rewritten" => {
                    session.move_note_with_directory_sync("a.md", "sub", None, &mut synchronize)
                }
                #[cfg(feature = "native-markdown-candidate")]
                "save_as_new" => {
                    // The production save-as entry cannot adopt an externally
                    // replaced destination merely because directory sync succeeded.
                    let document = session.open_document_buffer("a.md").unwrap();
                    session.save_document_buffer_as_new_with_directory_sync(
                        &document,
                        "",
                        "NewCopy",
                        &mut synchronize,
                    )
                }
                _ => unreachable!("固定操作矩阵"),
            };
            assert!(
                matches!(result, Err(MarkdownWorkspaceError::RollbackUnconfirmed { cause }) if *cause == MarkdownWorkspaceError::StorageUnavailable)
            );
            assert_eq!(calls, 1);
            assert_eq!(fs::read(root.join("a.md")).unwrap(), original);
            assert_eq!(
                fs::read(&destination).unwrap(),
                b"EXTERNAL destination bytes"
            );
        }
    }
}

#[test]
/// If a destination disappears during otherwise successful sync, fail the move
/// and preserve its source; absence cannot stand in for a committable file.
fn rewritten_move_preserves_source_when_synchronized_destination_disappears() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("vault");
    fs::create_dir_all(root.join("sub")).unwrap();
    let original = b"before [asset](attachments/p.png)";
    fs::write(root.join("a.md"), original).unwrap();
    let workspace = MarkdownWorkspace::new(root.clone());
    let session = workspace.open_session().unwrap();
    let result = session.move_note_with_directory_sync("a.md", "sub", None, |_| {
        // Remove only the synthetic destination as the fault input; no retry,
        // name recreation, or application-lifecycle action changes the outcome.
        fs::remove_file(root.join("sub/a.md")).unwrap();
        Ok(())
    });
    assert_eq!(fs::read(root.join("a.md")).unwrap(), original);
    assert!(!root.join("sub/a.md").exists());
    // Unlink changed the acquired FD's link identity. Strict rollback refuses
    // to reacquire it and preserves cleanup uncertainty with the original storage cause.
    assert!(
        matches!(result, Err(MarkdownWorkspaceError::RollbackUnconfirmed { cause }) if *cause == MarkdownWorkspaceError::StorageUnavailable)
    );
}
