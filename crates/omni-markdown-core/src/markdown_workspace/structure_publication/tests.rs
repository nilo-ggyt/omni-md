//! 实际合成文件变化后的失败反例：同时检查原权限和操作中途才建立的新权限。

use super::super::*;
use super::*;
use std::collections::HashSet;
use std::sync::atomic::AtomicBool;

/// 取得真实公开目录/正文搜索页，末页也有独立authority，不用伪造计数当作已发布。
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

/// 两类权限必须一起失效，不能只检查search掩盖目录漏撤。
fn assert_revoked(
    authority: &(MarkdownWorkspacePageAuthority, MarkdownSearchAuthority),
    session: &MarkdownSessionAccess,
) {
    assert!(!authority.0.is_current(session, &HashSet::new()));
    assert!(!authority.1.is_current(session, "before"));
}

#[cfg(feature = "native-markdown-candidate")]
#[test]
/// 所有结构路径在真实effect后注入同步Err，旧页和中途新页都不能越过终态。
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
        // 各分支独立fixture，避免上次撤权或遗留文件掩盖当前路径的漏接。
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("sub/child.md"), "before").unwrap();
        let content = if operation == "move_rewritten" {
            // 深度变化的附件链接强制进入创建重写目标再删除source的分支。
            "before [asset](attachments/pic.png)"
        } else {
            // 其他move保持正文不变，实际走独占rename分支。
            "before"
        };
        fs::write(root.join("a.md"), content).unwrap();
        fs::write(root.join("b.md"), "before").unwrap();
        let external = fixture.path().join("External.md");
        fs::write(&external, "before external").unwrap();
        let workspace = MarkdownWorkspace::new(root.clone(), fixture.path().join("data"));
        let session = workspace.open_session().unwrap();
        let old = authorities(&session);
        let mut mid = None;
        let mut called = 0;
        let mut fail_sync = |_: &Path| {
            called += 1;
            // 回调只位于真实发布之后；先核可见名字，再观察撤权，不假定operation成功返回。
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
            // 模拟worker在首次effect后才启动；终态不能放行它看到的临时/部分集合。
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
                // 另存的buffer仍关联原a.md，失败只能清理新目标，不能修改原正文/历史。
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
            // 这些分支仍有原rollback责任；终态撤权必须发生在guard移除候选之后。
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
/// O_EXCL后partial写失败必须保留无法证明的字节，终态仍撤销操作中途的新页。
fn created_note_write_failure_and_rollback_revoke_both_generations() {
    let fixture = tempfile::tempdir().unwrap();
    fs::write(fixture.path().join("a.md"), "before").unwrap();
    let workspace = MarkdownWorkspace::new(fixture.path().to_owned(), fixture.path().join("data"));
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
            // create已经成功且通知撤权，实际写部分字节使故障发生在真实副作用之后。
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
/// std递归的私有seam真实删掉一个子项后Err，目录仍存在且终态撤销中途页。
fn partial_recursive_delete_error_is_conservatively_revoked_without_reimplementing_std() {
    let fixture = tempfile::tempdir().unwrap();
    fs::create_dir(fixture.path().join("sub")).unwrap();
    fs::write(fixture.path().join("a.md"), "before").unwrap();
    fs::write(fixture.path().join("sub/removed.md"), "before").unwrap();
    fs::write(fixture.path().join("sub/kept.md"), "before").unwrap();
    let workspace = MarkdownWorkspace::new(fixture.path().to_owned(), fixture.path().join("data"));
    let session = workspace.open_session().unwrap();
    let old = authorities(&session);
    let mut mid = None;
    let result = session.delete_directory_with_removal(
        "sub",
        |_| panic!("失败移除不能进入sync"),
        |path| {
            // 删除尝试在进入opaque std之前保守撤权，这里再制造并观察真实partial变化。
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
/// 源post-read重验失败会drop返回中的候选guard，真实rollback和末端都不能漏撤。
fn source_revalidation_failure_releases_provisional_note_and_revokes_mid_page() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("vault");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("a.md"), "before").unwrap();
    let source = fixture.path().join("source.md");
    fs::write(&source, "before source").unwrap();
    let workspace = MarkdownWorkspace::new(root.clone(), fixture.path().join("data"));
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
        // 修改真实源长度强制post-read identity失败，目标候选已真实创建而非虚构mutation。
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
/// 未进入实际删除的根保护和真正no-op不激活终态guard，不无故取消有效页。
fn preflight_rejection_and_noop_leave_authority_current() {
    let fixture = tempfile::tempdir().unwrap();
    fs::create_dir(fixture.path().join("sub")).unwrap();
    fs::write(fixture.path().join("a.md"), "before").unwrap();
    let workspace = MarkdownWorkspace::new(fixture.path().to_owned(), fixture.path().join("data"));
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
/// 完整候选被外部替换或同inode改写后，四条真实rollback路径都保留外部字节和原首因。
fn provisional_note_rollback_preserves_replacements_and_same_inode_modifications() {
    for operation in ["import", "copy", "move_rewritten", "save_as_new"] {
        for replacement in [true, false] {
            // 每个反例重新取得文件与权限，不能靠上一例留下的目录状态绕过清理。
            let fixture = tempfile::tempdir().unwrap();
            let root = fixture.path().join("vault");
            fs::create_dir_all(root.join("sub")).unwrap();
            fs::write(root.join("a.md"), "before [asset](attachments/p.png)").unwrap();
            let source = fixture.path().join("External.md");
            fs::write(&source, "before external").unwrap();
            let workspace = MarkdownWorkspace::new(root.clone(), fixture.path().join("data"));
            let session = workspace.open_session().unwrap();
            let destination = root.join(match operation {
                // 各公开操作实际取得不同命名规则，测试不直接构造guard冒充调用链。
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
                        // 真正新inode通过rename接管同名路径；原FD仍属于已经退出目录的旧文件。
                        let external = root.join("external-replacement.tmp");
                        fs::write(&external, b"EXTERNAL replacement bytes").unwrap();
                        fs::rename(external, &destination).unwrap();
                        assert_ne!(fs::metadata(&destination).unwrap().ino(), acquired_inode);
                    } else {
                        // 原inode直接写入不同长度正文，ctime/mtime/size至少一项改变。
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
                    // 错误发生在完整取得和封存之后，Drop必须拒绝清理失去身份的候选。
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
                // 原来的完整bytes不再授权删除当前路径上的独立replacement。
                b"EXTERNAL replacement bytes"
            } else {
                // 同inode并非足够的清理依据，修改后的完整bytes同样保留。
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
/// 增加硬链接或替换为symlink都撤销清理权限；不会跟随链接删除外部文件。
fn provisional_note_rollback_rejects_added_aliases_and_symlinks() {
    use std::os::unix::fs::symlink;
    for symbolic in [false, true] {
        // 只测试本次取得的完整文件；外部目标使用独立路径和固定合成bytes。
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("a.md"), "before").unwrap();
        let workspace = MarkdownWorkspace::new(root.clone(), fixture.path().join("data"));
        let session = workspace.open_session().unwrap();
        let destination = root.join("a 副本.md");
        let external = fixture.path().join("external.md");
        let result = session.copy_note_with_directory_sync("a.md", None, |_| {
            if symbolic {
                // 同名symlink从不授予原取得FD对目标的unlink权限。
                fs::write(&external, b"external target").unwrap();
                fs::remove_file(&destination).unwrap();
                symlink(&external, &destination).unwrap();
            } else {
                // nlink变化说明已有额外观察者，不能删除原名字冒称完整回滚。
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
/// 父目录已移到库外再symlink接回时，最终file身份相同也不授权库外清理。
fn provisional_note_rollback_rejects_moved_parent_reconnected_by_symlink() {
    use std::os::unix::fs::symlink;
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("vault");
    let original_parent = root.join("sub");
    let moved_parent = fixture.path().join("outside");
    fs::create_dir_all(&original_parent).unwrap();
    fs::write(original_parent.join("a.md"), b"before original").unwrap();
    let workspace = MarkdownWorkspace::new(root, fixture.path().join("data"));
    let session = workspace.open_session().unwrap();
    let result = session.copy_note_with_directory_sync("sub/a.md", None, |_| {
        // 保留原file inode和bytes，只改变父目录的实际confinement。
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
/// 写入中途samepath替换后返回Err也不能删除外部replacement；没有完整seal可供清理。
fn partial_writer_failure_does_not_remove_external_replacement() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("vault");
    fs::create_dir(&root).unwrap();
    let destination = root.join("Partial.md");
    let workspace = MarkdownWorkspace::new(root, fixture.path().join("data"));
    let session = workspace.open_session().unwrap();
    let publication = WorkspaceStructurePublication::new(&session);
    let result = create_new_markdown_file_with_writer(
        &publication,
        &destination,
        b"complete expected",
        None,
        |file, _| {
            // 确实已有本次partial副作用；随后新inode接管路径，再注入真实写阶段失败。
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
/// rewrite move的source unlink是第二个真实发布点；其后sync失败保留目标并再次撤销中途权限。
fn rewritten_move_second_publication_revokes_intermediate_pages_even_when_final_sync_fails() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("vault");
    fs::create_dir_all(root.join("sub")).unwrap();
    fs::write(root.join("a.md"), "before [asset](attachments/p.png)").unwrap();
    let workspace = MarkdownWorkspace::new(root.clone(), fixture.path().join("data"));
    let session = workspace.open_session().unwrap();
    let mut first = None;
    let mut second = None;
    let mut calls = 0;
    let result = session.move_note_with_directory_sync("a.md", "sub", None, |_| {
        calls += 1;
        if calls == 1 {
            // destination已完整创建，但source仍在；模拟第一轮可见集合的真实worker权限。
            assert!(root.join("a.md").is_file());
            assert!(root.join("sub/a.md").is_file());
            first = Some(authorities(&session));
            Ok(())
        } else {
            // source已实际unlink；其即时撤权应先于第二次目录sync。
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
/// 即使调用者没有乐观版本，改写后的移动也不能删除同步期间外部更新的源文件。
fn rewritten_move_rechecks_its_own_source_revision_without_caller_precondition() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("vault");
    fs::create_dir_all(root.join("sub")).unwrap();
    let source = root.join("a.md");
    fs::write(&source, b"before [asset](attachments/p.png)").unwrap();
    let workspace = MarkdownWorkspace::new(root.clone(), fixture.path().join("data"));
    let session = workspace.open_session().unwrap();
    let mut calls = 0;
    let result = session.move_note_with_directory_sync("a.md", "sub", None, |_| {
        calls += 1;
        // 目标已完整封存；模拟外部程序在目录同步中途成功更新源，回调仍返回成功。
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
/// 成功的目录同步回调不证明目标身份；外部改写和同名替换均保留源与外部目标。
fn successful_sync_does_not_commit_a_modified_provisional_note() {
    for operation in [
        "import",
        "copy",
        "move_rewritten",
        #[cfg(feature = "native-markdown-candidate")]
        "save_as_new",
    ] {
        for replacement in [false, true] {
            // 独立的真实文件系统fixture，避免一次失败残留改变下一操作的命名或版本。
            let fixture = tempfile::tempdir().unwrap();
            let root = fixture.path().join("vault");
            fs::create_dir_all(root.join("sub")).unwrap();
            let original = b"before [asset](attachments/p.png)";
            fs::write(root.join("a.md"), original).unwrap();
            let external_source = fixture.path().join("External.md");
            fs::write(&external_source, original).unwrap();
            let workspace = MarkdownWorkspace::new(root.clone(), fixture.path().join("data"));
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
                    // 独立inode替换路径，原取得FD仍持旧值，不能授权删除新对象。
                    let temporary = root.join("external.tmp");
                    fs::write(&temporary, b"EXTERNAL destination bytes").unwrap();
                    fs::rename(temporary, &destination).unwrap();
                } else {
                    // 同inode的正文变化同样撤销封存身份，不能因为路径未变就提交。
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
                    // 同一生产另存入口在成功同步之后也不能认领外部替换的目标。
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
/// 目标在成功同步期间消失时，移动失败并保留源；缺失名字不会被当作可提交文件。
fn rewritten_move_preserves_source_when_synchronized_destination_disappears() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("vault");
    fs::create_dir_all(root.join("sub")).unwrap();
    let original = b"before [asset](attachments/p.png)";
    fs::write(root.join("a.md"), original).unwrap();
    let workspace = MarkdownWorkspace::new(root.clone(), fixture.path().join("data"));
    let session = workspace.open_session().unwrap();
    let result = session.move_note_with_directory_sync("a.md", "sub", None, |_| {
        // 删除只是合成故障输入；没有额外重试、复活名字或应用内关闭动作。
        fs::remove_file(root.join("sub/a.md")).unwrap();
        Ok(())
    });
    assert_eq!(fs::read(root.join("a.md")).unwrap(), original);
    assert!(!root.join("sub/a.md").exists());
    // unlink已改变原取得FD的链接身份，严格回滚拒绝重认领；保留不确定清理与原存储原因。
    assert!(
        matches!(result, Err(MarkdownWorkspaceError::RollbackUnconfirmed { cause }) if *cause == MarkdownWorkspaceError::StorageUnavailable)
    );
}
