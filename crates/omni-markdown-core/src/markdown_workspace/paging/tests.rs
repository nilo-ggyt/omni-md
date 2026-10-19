//! 合成目录验证跨页总序、Pending边界、折叠与撤权，不读取用户正文。

use super::*;

/// 真实公开分页器按有限步骤完成一页，测试不能把Pending当空页消费。
fn finish_page(
    pager: &mut MarkdownWorkspacePager,
    limit: usize,
    budget: usize,
    collapsed: &HashSet<String>,
) -> MarkdownWorkspacePage {
    for _ in 0..10_000 {
        // fixture有限；固定上限只防止实现不前进导致测试永久阻塞。
        let progress = pager
            .next_page_cancellable(limit, budget, collapsed, &AtomicBool::new(false))
            .expect("合成目录扫描成功");
        assert!(pager.candidates.len() <= limit + 1);
        assert!(pager.candidates.capacity() <= limit + 1);
        match progress {
            MarkdownWorkspacePageProgress::Pending => {
                // 候选不外露，继续有限扫描步骤。
            }
            MarkdownWorkspacePageProgress::Ready(page) => return page,
        }
    }
    panic!("有限fixture必须完成目录扫描");
}

/// 快照分组仅是运输形式；消费者使用公开比较器恢复同一树总序。
fn ordered_paths(page: &MarkdownWorkspacePage) -> Vec<String> {
    let mut entries: Vec<_> = page
        .snapshot
        .directories
        .iter()
        .map(|path| (path.as_str(), MarkdownWorkspaceEntryKind::Directory))
        .chain(page.snapshot.notes.iter().map(|note| {
            (
                note.relative_path.as_str(),
                MarkdownWorkspaceEntryKind::Note,
            )
        }))
        .collect();
    entries.sort_by(|left, right| {
        compare_markdown_workspace_entries(left.0, left.1, right.0, right.1)
    });
    entries
        .into_iter()
        .map(|(path, _)| path.to_owned())
        .collect()
}

#[test]
/// 文件创建顺序与排序逆序，55项跨五页仍全局升序且唯一；正文故意不是UTF8。
fn fifty_five_notes_are_globally_sorted_across_five_bounded_pages() {
    let fixture = tempfile::tempdir().unwrap();
    for index in (0..55).rev() {
        // 目录只需metadata；若误走正文解码，此fixture会失败。
        fs::write(
            fixture.path().join(format!("note-{index:02}.md")),
            [0xff, 0xfe],
        )
        .unwrap();
    }
    let workspace = MarkdownWorkspace::new(fixture.path().to_owned(), fixture.path().join("data"));
    let session = workspace.open_session().unwrap();
    let mut pager = session.open_workspace_pager().unwrap();
    let mut all = Vec::new();
    let mut sizes = Vec::new();
    loop {
        // 小步预算强制每页经过多个Pending，避免只覆盖单次read_dir的小目录路径。
        let page = finish_page(&mut pager, 12, 7, &HashSet::new());
        assert!(page.authority.is_current(&session, &HashSet::new()));
        let paths = ordered_paths(&page);
        sizes.push(paths.len());
        all.extend(paths);
        if !page.has_more {
            // 真实K+1耗尽结束，不按预设页数提前停止。
            break;
        }
    }
    assert_eq!(sizes, [12, 12, 12, 12, 7]);
    assert_eq!(
        all,
        (0..55)
            .map(|index| format!("note-{index:02}.md"))
            .collect::<Vec<_>>()
    );
    assert!(pager.stack.is_empty());
    assert_eq!(pager.candidates.capacity(), 0);
}

#[test]
/// 树中每层目录优先，父先于子；大小写/中文/emoji按声明scalar序而非拼音。
fn nested_tree_order_and_unicode_names_do_not_depend_on_readdir_order() {
    let fixture = tempfile::tempdir().unwrap();
    for directory in ["z/child", "a/中", "a/Z"] {
        fs::create_dir_all(fixture.path().join(directory)).unwrap();
    }
    for note in [
        "z/a.md",
        "a/z.md",
        "a/中/内.md",
        "a/Z/😀.md",
        "中.md",
        "文.md",
        "Zz.md",
        "aB.md",
        "😀.md",
    ] {
        fs::write(fixture.path().join(note), []).unwrap();
    }
    let workspace = MarkdownWorkspace::new(fixture.path().to_owned(), fixture.path().join("data"));
    let session = workspace.open_session().unwrap();
    let mut pager = session.open_workspace_pager().unwrap();
    let mut all = Vec::new();
    loop {
        // 两行页宽把父、子和同名种类放到不同页，直接覆盖跨页比较。
        let page = finish_page(&mut pager, 2, 3, &HashSet::new());
        all.extend(ordered_paths(&page));
        if !page.has_more {
            break;
        }
    }
    assert_eq!(
        all,
        [
            "a",
            "a/Z",
            "a/Z/😀.md",
            "a/中",
            "a/中/内.md",
            "a/z.md",
            "z",
            "z/child",
            "z/a.md",
            "Zz.md",
            "aB.md",
            "中.md",
            "文.md",
            "😀.md"
        ]
    );
}

#[test]
/// 512是硬步骤上限；还没扫完整库时没有随机前页可安装。
fn pending_does_not_publish_a_random_prefix_even_with_unbounded_requested_scan() {
    let fixture = tempfile::tempdir().unwrap();
    for index in 0..1_025 {
        // 忽略文件也收费；大量无关条目不能使一步扫描越界。
        fs::write(fixture.path().join(format!("ignored-{index}.txt")), []).unwrap();
    }
    fs::write(fixture.path().join("first.md"), []).unwrap();
    let workspace = MarkdownWorkspace::new(fixture.path().to_owned(), fixture.path().join("data"));
    let session = workspace.open_session().unwrap();
    let mut pager = session.open_workspace_pager().unwrap();
    for _ in 0..2 {
        // 即使外部传usize::MAX，前两步仍不足以消费1026项和EOF。
        assert!(matches!(
            pager
                .next_page_cancellable(12, usize::MAX, &HashSet::new(), &AtomicBool::new(false))
                .unwrap(),
            MarkdownWorkspacePageProgress::Pending
        ));
    }
    let page = finish_page(&mut pager, 12, 512, &HashSet::new());
    assert_eq!(ordered_paths(&page), ["first.md"]);
    assert!(!page.has_more);
}

#[test]
/// 折叠、附件和链接过滤仍生效；中途改折叠不能沿旧末项继续。
fn collapse_changes_require_refresh_and_do_not_traverse_reserved_or_linked_trees() {
    let fixture = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    fs::create_dir_all(fixture.path().join("folder/sub")).unwrap();
    fs::create_dir(fixture.path().join("attachments")).unwrap();
    fs::write(fixture.path().join("folder/sub/hidden.md"), []).unwrap();
    fs::write(fixture.path().join("attachments/asset.md"), []).unwrap();
    fs::write(external.path().join("outside.md"), []).unwrap();
    std::os::unix::fs::symlink(external.path(), fixture.path().join("linked")).unwrap();
    let workspace = MarkdownWorkspace::new(fixture.path().to_owned(), fixture.path().join("data"));
    let session = workspace.open_session().unwrap();
    let mut pager = session.open_workspace_pager().unwrap();
    assert!(matches!(
        pager
            .next_page_cancellable(12, 1, &HashSet::new(), &AtomicBool::new(false))
            .unwrap(),
        MarkdownWorkspacePageProgress::Pending
    ));
    let collapsed = HashSet::from(["folder".to_owned()]);
    assert!(matches!(
        pager.next_page_cancellable(12, 512, &collapsed, &AtomicBool::new(false)),
        Err(MarkdownWorkspaceError::InvalidWorkspaceCursor)
    ));
    assert!(pager.stack.is_empty());
    let mut fresh = session.open_workspace_pager().unwrap();
    let page = finish_page(&mut fresh, 12, 2, &collapsed);
    assert_eq!(ordered_paths(&page), ["folder"]);
    assert!(!page.authority.is_current(&session, &HashSet::new()));
}

#[test]
/// 取消在步骤之间撤权并释放半页；复位flag不能复活被取消游标。
fn cancellation_releases_pending_payload_and_cannot_be_reset_into_late_delivery() {
    let fixture = tempfile::tempdir().unwrap();
    for index in 0..55 {
        fs::write(fixture.path().join(format!("{index}.md")), []).unwrap();
    }
    let workspace = MarkdownWorkspace::new(fixture.path().to_owned(), fixture.path().join("data"));
    let session = workspace.open_session().unwrap();
    let mut pager = session.open_workspace_pager().unwrap();
    let cancelled = AtomicBool::new(false);
    assert!(matches!(
        pager
            .next_page_cancellable(12, 4, &HashSet::new(), &cancelled)
            .unwrap(),
        MarkdownWorkspacePageProgress::Pending
    ));
    cancelled.store(true, AtomicOrdering::Release);
    assert!(matches!(
        pager.next_page_cancellable(12, 4, &HashSet::new(), &cancelled),
        Err(MarkdownWorkspaceError::WorkspaceSuperseded)
    ));
    assert!(pager.stack.is_empty());
    assert_eq!(pager.candidates.capacity(), 0);
    cancelled.store(false, AtomicOrdering::Release);
    assert!(matches!(
        pager.next_page_cancellable(12, 4, &HashSet::new(), &cancelled),
        Err(MarkdownWorkspaceError::WorkspaceSuperseded)
    ));
}

#[test]
/// 新目录刷新只撤旧目录，末页没有next也必须失去旧安装权限。
fn refresh_and_local_mutations_revoke_final_page_authority() {
    let fixture = tempfile::tempdir().unwrap();
    fs::write(fixture.path().join("a.md"), []).unwrap();
    let workspace = MarkdownWorkspace::new(fixture.path().to_owned(), fixture.path().join("data"));
    let session = workspace.open_session().unwrap();
    let mut old = session.open_workspace_pager().unwrap();
    let final_page = finish_page(&mut old, 12, 512, &HashSet::new());
    assert!(!final_page.has_more);
    let mut refreshed = session.open_workspace_pager().unwrap();
    assert!(!final_page.authority.is_current(&session, &HashSet::new()));
    let page = finish_page(&mut refreshed, 12, 512, &HashSet::new());
    session.create_note("", "b").unwrap();
    assert!(!page.authority.is_current(&session, &HashSet::new()));
    assert!(matches!(
        refreshed.next_page_cancellable(12, 512, &HashSet::new(), &AtomicBool::new(false)),
        Err(MarkdownWorkspaceError::WorkspaceSuperseded)
    ));
    let mut fresh = session.open_workspace_pager().unwrap();
    let page = finish_page(&mut fresh, 12, 512, &HashSet::new());
    assert_eq!(ordered_paths(&page), ["a.md", "b.md"]);
    workspace.close_session();
    assert!(!page.authority.is_current(&session, &HashSet::new()));
}

#[test]
/// 目录刷新不取消已展示搜索的cursor；搜索取消/本地发布仍阻断旧目录迟到交付。
fn directory_refresh_and_search_continuation_have_separate_authority() {
    let fixture = tempfile::tempdir().unwrap();
    for index in 0..15 {
        fs::write(fixture.path().join(format!("hit-{index:02}.md")), []).unwrap();
    }
    let workspace = MarkdownWorkspace::new(fixture.path().to_owned(), fixture.path().join("data"));
    let session = workspace.open_session().unwrap();
    let cancelled = AtomicBool::new(false);
    let search = session
        .search_notes_page_cancellable("hit", 5, None, &cancelled)
        .unwrap();
    let cursor = search.next.as_ref().unwrap();
    let mut pager = session.open_workspace_pager().unwrap();
    let directory = finish_page(&mut pager, 12, 512, &HashSet::new());
    assert!(search.authority.is_current(&session, "hit"));
    let next = session
        .search_notes_page_cancellable("hit", 5, Some(cursor), &cancelled)
        .unwrap();
    assert_eq!(next.results.first().unwrap().relative_path, "hit-05.md");
    assert!(directory.authority.is_current(&session, &HashSet::new()));
    session.cancel_search();
    assert!(!directory.authority.is_current(&session, &HashSet::new()));
    assert!(!next.authority.is_current(&session, "hit"));
}

#[test]
/// 同一路径重建根或换页宽不能把旧key套到新集合；旧close不得撤销较新pager。
fn stale_root_page_width_and_late_close_are_rejected_without_revoking_new_pager() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("vault");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("a.md"), []).unwrap();
    let workspace = MarkdownWorkspace::new(root.clone(), fixture.path().join("data"));
    let session = workspace.open_session().unwrap();
    let mut old = session.open_workspace_pager().unwrap();
    let _ = finish_page(&mut old, 1, 512, &HashSet::new());
    let mut fresh = session.open_workspace_pager().unwrap();
    old.close();
    let page = finish_page(&mut fresh, 12, 512, &HashSet::new());
    assert!(page.authority.is_current(&session, &HashSet::new()));
    assert!(matches!(
        fresh.next_page_cancellable(11, 512, &HashSet::new(), &AtomicBool::new(false)),
        Err(MarkdownWorkspaceError::InvalidWorkspaceCursor)
    ));
    let mut replaced = session.open_workspace_pager().unwrap();
    fs::rename(&root, fixture.path().join("old-vault")).unwrap();
    fs::create_dir(&root).unwrap();
    assert!(matches!(
        replaced.next_page_cancellable(12, 512, &HashSet::new(), &AtomicBool::new(false)),
        Err(MarkdownWorkspaceError::InvalidWorkspaceCursor)
    ));
}

#[test]
/// 不依赖本机文件系统能否同时创建NFC/NFD或扩展大小写别名，纯核公开总序。
fn unicode_and_display_name_ties_remain_a_strict_total_order() {
    let note = MarkdownWorkspaceEntryKind::Note;
    let directory = MarkdownWorkspaceEntryKind::Directory;
    let expected = [
        ("a", directory),
        ("a/b.md", note),
        ("Z.md", note),
        ("a.MD", note),
        ("a.md", note),
        ("e\u{301}.md", note),
        ("é.md", note),
        ("中.md", note),
    ];
    for (left_index, left) in expected.iter().enumerate() {
        for (right_index, right) in expected.iter().enumerate() {
            // 所有两两关系核对一个人工声明顺序，含同stem不同真实路径的平局。
            assert_eq!(
                compare_markdown_workspace_entries(left.0, left.1, right.0, right.1),
                left_index.cmp(&right_index)
            );
        }
    }
}

#[cfg(feature = "native-markdown-candidate")]
#[test]
/// 同路径保存保持55条目录的after与Pending进度，旧正文搜索仍须撤权并查到新正文。
fn native_and_legacy_content_saves_preserve_directory_continuation_but_revoke_search() {
    let fixture = tempfile::tempdir().unwrap();
    for index in 0..55 {
        // 固定名称和初始正文分离，正文变化不应改变目录中的排名。
        fs::write(fixture.path().join(format!("note-{index:02}.md")), "before").unwrap();
    }
    let workspace = MarkdownWorkspace::new(fixture.path().to_owned(), fixture.path().join("data"));
    let session = workspace.open_session().unwrap();
    let mut document = session.open_document_buffer("note-20.md").unwrap();
    let mut pager = session.open_workspace_pager().unwrap();
    let page = finish_page(&mut pager, 12, 512, &HashSet::new());
    assert_eq!(
        ordered_paths(&page),
        (0..12)
            .map(|index| format!("note-{index:02}.md"))
            .collect::<Vec<_>>()
    );
    let cancelled = AtomicBool::new(false);
    let search = session
        .search_notes_page_cancellable("before", 1, None, &cancelled)
        .unwrap();
    let root_inode = fs::metadata(fixture.path()).unwrap().ino();
    let original_note_inode = fs::metadata(fixture.path().join("note-20.md"))
        .unwrap()
        .ino();
    session.save_document_buffer(&mut document).unwrap();
    // no-op没有发布，目录和搜索的两种原权限都保留。
    assert!(page.authority.is_current(&session, &HashSet::new()));
    assert!(search.authority.is_current(&session, "before"));
    document
        .replace_utf16_range_with_selection(
            0..6,
            "after",
            crate::MarkdownTextSelection::caret(0),
            crate::MarkdownTextSelection::caret(5),
        )
        .unwrap();
    session.save_document_buffer(&mut document).unwrap();
    // 真实原子替换换了note inode，但root/path/kind/stem/collapse未变。
    assert_ne!(
        fs::metadata(fixture.path().join("note-20.md"))
            .unwrap()
            .ino(),
        original_note_inode
    );
    assert_eq!(fs::metadata(fixture.path()).unwrap().ino(), root_inode);
    assert!(page.authority.is_current(&session, &HashSet::new()));
    assert!(!search.authority.is_current(&session, "before"));
    let refreshed_search = session
        .search_notes_page_cancellable("after", 13, None, &cancelled)
        .unwrap();
    assert_eq!(
        refreshed_search
            .results
            .iter()
            .map(|note| note.relative_path.as_str())
            .collect::<Vec<_>>(),
        ["note-20.md"]
    );
    let second = finish_page(&mut pager, 12, 7, &HashSet::new());
    assert_eq!(
        ordered_paths(&second),
        (12..24)
            .map(|index| format!("note-{index:02}.md"))
            .collect::<Vec<_>>()
    );
    assert!(matches!(
        pager
            .next_page_cancellable(12, 1, &HashSet::new(), &AtomicBool::new(false))
            .unwrap(),
        MarkdownWorkspacePageProgress::Pending
    ));
    let legacy = session.open_note("note-30.md").unwrap();
    session
        .save_note("note-30.md", "after", &legacy.revision)
        .unwrap();
    // Pending不会因自动保存丢掉已扫进度或跳回第一页；搜索正文权限仍正确撤销。
    assert!(second.authority.is_current(&session, &HashSet::new()));
    assert!(!refreshed_search.authority.is_current(&session, "after"));
    let third = finish_page(&mut pager, 12, 7, &HashSet::new());
    assert_eq!(
        ordered_paths(&third),
        (24..36)
            .map(|index| format!("note-{index:02}.md"))
            .collect::<Vec<_>>()
    );
    let refreshed_search = session
        .search_notes_page_cancellable("after", 13, None, &cancelled)
        .unwrap();
    assert_eq!(
        refreshed_search
            .results
            .iter()
            .map(|note| note.relative_path.as_str())
            .collect::<Vec<_>>(),
        ["note-20.md", "note-30.md"]
    );
    let fourth = finish_page(&mut pager, 12, 7, &HashSet::new());
    let fifth = finish_page(&mut pager, 12, 7, &HashSet::new());
    assert_eq!(
        ordered_paths(&fourth),
        (36..48)
            .map(|index| format!("note-{index:02}.md"))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        ordered_paths(&fifth),
        (48..55)
            .map(|index| format!("note-{index:02}.md"))
            .collect::<Vec<_>>()
    );
    assert!(!fifth.has_more);
    assert!(fifth.authority.is_current(&session, &HashSet::new()));
}

#[test]
/// legacy真实rename后即使dirsync返回Err，也只撤正文搜索，目录仍从原末项继续。
fn legacy_post_rename_sync_failure_preserves_browse_and_revokes_search_before_error() {
    let fixture = tempfile::tempdir().unwrap();
    fs::write(fixture.path().join("a.md"), "before").unwrap();
    fs::write(fixture.path().join("b.md"), "before").unwrap();
    let workspace = MarkdownWorkspace::new(fixture.path().to_owned(), fixture.path().join("data"));
    let session = workspace.open_session().unwrap();
    let original = session.open_note("a.md").unwrap();
    let mut pager = session.open_workspace_pager().unwrap();
    let page = finish_page(&mut pager, 1, 512, &HashSet::new());
    let search = session
        .search_notes_page_cancellable("before", 1, None, &AtomicBool::new(false))
        .unwrap();
    let result =
        session.save_note_with_directory_sync("a.md", "after", &original.revision, |parent| {
            // 这个回调只在真实rename和撤权之后调用，不靠预设counter假装已发布。
            assert_eq!(parent, fixture.path().canonicalize().unwrap());
            assert_eq!(fs::read(fixture.path().join("a.md")).unwrap(), b"after");
            assert!(!search.authority.is_current(&session, "before"));
            assert!(page.authority.is_current(&session, &HashSet::new()));
            Err(MarkdownWorkspaceError::StorageUnavailable)
        });
    assert_eq!(result, Err(MarkdownWorkspaceError::StorageUnavailable));
    assert!(!search.authority.is_current(&session, "before"));
    assert!(page.authority.is_current(&session, &HashSet::new()));
    let next = finish_page(&mut pager, 1, 512, &HashSet::new());
    assert_eq!(ordered_paths(&next), ["b.md"]);
    assert!(!next.has_more);
}

#[cfg(feature = "native-markdown-candidate")]
#[test]
/// 结构操作仍同时撤销目录与搜索，不能把所有保存/文件操作宽泛归为content-only。
fn structural_publications_continue_to_revoke_both_directory_and_search() {
    for operation in ["create", "rename", "move", "delete", "save_as_new"] {
        // 每个操作独立fixture和权限，避免先前撤权掩盖后面漏接的分支。
        let fixture = tempfile::tempdir().unwrap();
        fs::create_dir(fixture.path().join("sub")).unwrap();
        fs::write(fixture.path().join("a.md"), "before").unwrap();
        let workspace =
            MarkdownWorkspace::new(fixture.path().to_owned(), fixture.path().join("data"));
        let session = workspace.open_session().unwrap();
        let mut pager = session.open_workspace_pager().unwrap();
        let page = finish_page(&mut pager, 12, 512, &HashSet::new());
        let search = session
            .search_notes_page_cancellable("before", 1, None, &AtomicBool::new(false))
            .unwrap();
        match operation {
            "create" => {
                // 新路径进入排序集合，旧目录和搜索都失效。
                session.create_note("", "new").unwrap();
            }
            "rename" => {
                // 文件stem和排序位置发生变化，不能继续沿旧末项交付。
                session.rename_note("a.md", "renamed", None).unwrap();
            }
            "move" => {
                // 父路径改变影响树层级，仍使用结构发布撤权。
                session.move_note("a.md", "sub", None).unwrap();
            }
            "delete" => {
                // 条目退出集合，排队目录页和正文搜索都不得再安装。
                session.delete_note("a.md", None).unwrap();
            }
            "save_as_new" => {
                // 另存新增真实路径，和保持路径的正文保存使用不同撤权合同。
                let document = session.open_document_buffer("a.md").unwrap();
                session
                    .save_document_buffer_as_new(&document, "", "copy")
                    .unwrap();
            }
            _ => {
                // 测试枚举由上面的固定表给定，未知项不能悄悄跳过断言。
                unreachable!("测试操作集合固定")
            }
        }
        assert!(
            !page.authority.is_current(&session, &HashSet::new()),
            "{operation}"
        );
        assert!(
            !search.authority.is_current(&session, "before"),
            "{operation}"
        );
    }
}
