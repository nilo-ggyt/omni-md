//! Synthetic directories verify cross-page order, Pending, collapse intent,
//! and revocation without reading any user vault or original document.

use super::*;

/// Complete a real public pager in finite steps; Pending is never consumed as
/// an empty successful page.
fn finish_page(
    pager: &mut MarkdownWorkspacePager,
    limit: usize,
    budget: usize,
    collapsed: &HashSet<String>,
) -> MarkdownWorkspacePage {
    for _ in 0..10_000 {
        // The fixture is finite; this iteration ceiling prevents a nonprogressing
        // implementation from blocking the regression indefinitely.
        let progress = pager
            .next_page_cancellable(limit, budget, collapsed, &AtomicBool::new(false))
            .expect("合成目录扫描成功");
        assert!(pager.candidates.len() <= limit + 1);
        assert!(pager.candidates.capacity() <= limit + 1);
        match progress {
            MarkdownWorkspacePageProgress::Pending => {
                // No candidates escaped; continue with another bounded real scan step.
            }
            // Only a completed scan may return the public page to the caller.
            MarkdownWorkspacePageProgress::Ready(page) => return page,
        }
    }
    panic!("有限fixture必须完成目录扫描");
}

/// Snapshot grouping is transport only; reconstruct the mixed tree order with
/// the same public comparator used by its native consumer.
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
/// Reverse creation order and invalid UTF-8 bodies isolate metadata-only paging:
/// all 55 entries must remain unique and globally ordered across five pages.
fn fifty_five_notes_are_globally_sorted_across_five_bounded_pages() {
    let fixture = tempfile::tempdir().unwrap();
    for index in (0..55).rev() {
        // Browsing needs metadata only; accidental body decoding fails this fixture.
        fs::write(
            fixture.path().join(format!("note-{index:02}.md")),
            [0xff, 0xfe],
        )
        .unwrap();
    }
    let workspace = MarkdownWorkspace::new(fixture.path().to_owned());
    let session = workspace.open_session().unwrap();
    let mut pager = session.open_workspace_pager().unwrap();
    let mut all = Vec::new();
    let mut sizes = Vec::new();
    loop {
        // Small scan budgets force multiple Pending states per page rather than
        // covering only directories that fit in one read_dir step.
        let page = finish_page(&mut pager, 12, 7, &HashSet::new());
        assert!(page.authority.is_current(&session, &HashSet::new()));
        let paths = ordered_paths(&page);
        sizes.push(paths.len());
        all.extend(paths);
        if !page.has_more {
            // Stop on real K+1 exhaustion, not an assumed page count that could lose results.
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
/// Keep directories first at every depth and parents before children, with case,
/// CJK, and emoji names following the declared scalar order rather than pinyin.
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
    let workspace = MarkdownWorkspace::new(fixture.path().to_owned());
    let session = workspace.open_session().unwrap();
    let mut pager = session.open_workspace_pager().unwrap();
    let mut all = Vec::new();
    loop {
        // Two-entry pages separate ancestors, descendants, and entry kinds so
        // the same comparator must remain correct across continuation boundaries.
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
/// A 512-step bound cannot publish an arbitrary prefix before complete traversal.
fn pending_does_not_publish_a_random_prefix_even_with_unbounded_requested_scan() {
    let fixture = tempfile::tempdir().unwrap();
    for index in 0..1_025 {
        // Ignored files still consume scan work; unrelated entries cannot bypass
        // a step's enumeration limit.
        fs::write(fixture.path().join(format!("ignored-{index}.txt")), []).unwrap();
    }
    fs::write(fixture.path().join("first.md"), []).unwrap();
    let workspace = MarkdownWorkspace::new(fixture.path().to_owned());
    let session = workspace.open_session().unwrap();
    let mut pager = session.open_workspace_pager().unwrap();
    for _ in 0..2 {
        // Even usize::MAX cannot let two steps consume 1,026 entries plus EOF.
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
/// Preserve collapse, asset-tree, and symlink filtering; changed collapse intent
/// cannot continue from a key bound to the previous visible tree.
fn collapse_changes_require_refresh_and_do_not_traverse_reserved_or_linked_trees() {
    let fixture = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    fs::create_dir_all(fixture.path().join("folder/sub")).unwrap();
    fs::create_dir(fixture.path().join("attachments")).unwrap();
    fs::write(fixture.path().join("folder/sub/hidden.md"), []).unwrap();
    fs::write(fixture.path().join("attachments/asset.md"), []).unwrap();
    fs::write(external.path().join("outside.md"), []).unwrap();
    std::os::unix::fs::symlink(external.path(), fixture.path().join("linked")).unwrap();
    let workspace = MarkdownWorkspace::new(fixture.path().to_owned());
    let session = workspace.open_session().unwrap();
    let mut pager = session.open_workspace_pager().unwrap();
    assert!(matches!(
        pager
            .next_page_cancellable(12, 1, &HashSet::new(), &AtomicBool::new(false))
            .unwrap(),
        MarkdownWorkspacePageProgress::Pending
    ));
    let collapsed = HashSet::from(["folder".to_owned()]);
    // Change intent during a real Pending traversal, then prove a fresh pager
    // includes the collapsed directory itself without descending into its children.
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
/// Cancellation between steps releases the partial page and permanently revokes
/// that cursor, even if the caller later resets its cancellation token.
fn cancellation_releases_pending_payload_and_cannot_be_reset_into_late_delivery() {
    let fixture = tempfile::tempdir().unwrap();
    for index in 0..55 {
        fs::write(fixture.path().join(format!("{index}.md")), []).unwrap();
    }
    let workspace = MarkdownWorkspace::new(fixture.path().to_owned());
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
/// A new traversal revokes the old terminal page even though it has no continuation.
fn refresh_and_local_mutations_revoke_final_page_authority() {
    let fixture = tempfile::tempdir().unwrap();
    fs::write(fixture.path().join("a.md"), []).unwrap();
    let workspace = MarkdownWorkspace::new(fixture.path().to_owned());
    let session = workspace.open_session().unwrap();
    let mut old = session.open_workspace_pager().unwrap();
    let final_page = finish_page(&mut old, 12, 512, &HashSet::new());
    assert!(!final_page.has_more);
    let mut refreshed = session.open_workspace_pager().unwrap();
    assert!(!final_page.authority.is_current(&session, &HashSet::new()));
    let page = finish_page(&mut refreshed, 12, 512, &HashSet::new());
    // Public creation and later session closure separately revoke completed
    // page authority; a fresh traversal alone observes the new actual note.
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
/// Browsing refresh preserves independent search continuation, while explicit
/// search cancellation revokes old browse delivery under the shared cancel contract.
fn directory_refresh_and_search_continuation_have_separate_authority() {
    let fixture = tempfile::tempdir().unwrap();
    for index in 0..15 {
        fs::write(fixture.path().join(format!("hit-{index:02}.md")), []).unwrap();
    }
    let workspace = MarkdownWorkspace::new(fixture.path().to_owned());
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
/// Recreated roots and changed widths reject old keys; closing an older pager
/// must not revoke a newer traversal's generation.
fn stale_root_page_width_and_late_close_are_rejected_without_revoking_new_pager() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("vault");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("a.md"), []).unwrap();
    let workspace = MarkdownWorkspace::new(root.clone());
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
/// Check the public total order independently of whether this filesystem permits
/// simultaneous NFC/NFD names or case aliases.
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
            // Compare all pairs with a hand-declared order, including equal display
            // stems whose distinct real paths must still break ties.
            assert_eq!(
                compare_markdown_workspace_entries(left.0, left.1, right.0, right.1),
                left_index.cmp(&right_index)
            );
        }
    }
}

#[cfg(feature = "native-markdown-candidate")]
#[test]
/// Same-path mmap saves preserve browse continuation while revoking searches of old body bytes.
fn content_saves_preserve_directory_continuation_but_revoke_search() {
    let fixture = tempfile::tempdir().unwrap();
    for index in 0..55 {
        // Stable paths keep their tree position even when a later public buffer transaction changes the body.
        fs::write(fixture.path().join(format!("note-{index:02}.md")), "before").unwrap();
    }
    let workspace = MarkdownWorkspace::new(fixture.path().to_owned());
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
    // A clean save publishes nothing: both the directory page and the body-search authority stay usable.
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
    // The real atomic save replaces the note inode, but not its path, kind, stem, root or collapse policy.
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
    // Change a second real mmap-backed document while the existing pager is between bounded scan steps.
    // Neither reading nor editing this buffer may materialize a second whole-document string API.
    let mut second_document = session.open_document_buffer("note-30.md").unwrap();
    second_document
        .replace_utf16_range_with_selection(
            0..6,
            "after",
            crate::MarkdownTextSelection::caret(0),
            crate::MarkdownTextSelection::caret(5),
        )
        .unwrap();
    session.save_document_buffer(&mut second_document).unwrap();
    // Saving body bytes cannot reset the Pending browse scan or replay its first page. A search
    // prepared before that real publication must nevertheless lose its body-dependent authority.
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

#[cfg(feature = "native-markdown-candidate")]
#[test]
/// Structural operations still revoke browsing and search together; not every
/// save/file operation qualifies for same-path content-only invalidation.
fn structural_publications_continue_to_revoke_both_directory_and_search() {
    for operation in ["create", "rename", "move", "delete", "save_as_new"] {
        // Give each real operation independent resources and authority so an
        // earlier revocation cannot conceal another operation's missing invalidation.
        let fixture = tempfile::tempdir().unwrap();
        fs::create_dir(fixture.path().join("sub")).unwrap();
        fs::write(fixture.path().join("a.md"), "before").unwrap();
        let workspace = MarkdownWorkspace::new(fixture.path().to_owned());
        let session = workspace.open_session().unwrap();
        let mut pager = session.open_workspace_pager().unwrap();
        let page = finish_page(&mut pager, 12, 512, &HashSet::new());
        let search = session
            .search_notes_page_cancellable("before", 1, None, &AtomicBool::new(false))
            .unwrap();
        match operation {
            "create" => {
                // A new path enters the sorted collection, revoking browse and search.
                session.create_note("", "new").unwrap();
            }
            "rename" => {
                // A changed stem/order position cannot continue delivery from an old key.
                session.rename_note("a.md", "renamed", None).unwrap();
            }
            "move" => {
                // A different parent changes tree order and requires structural revocation.
                session.move_note("a.md", "sub", None).unwrap();
            }
            "delete" => {
                // Removal ends membership; neither queued pages nor old body searches may install.
                session.delete_note("a.md", None).unwrap();
            }
            "save_as_new" => {
                // Save-as creates a real new path and uses structural authority,
                // unlike content persistence under an unchanged path.
                let document = session.open_document_buffer("a.md").unwrap();
                session
                    .save_document_buffer_as_new(&document, "", "copy")
                    .unwrap();
            }
            _ => {
                // Reject an unknown case instead of silently skipping this fixed
                // table's actual-operation assertions.
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
