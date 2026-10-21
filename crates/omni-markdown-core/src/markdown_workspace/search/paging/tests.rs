//! 使用真实临时库验证续页边界与取消；不读取用户目录。
use super::*;

/// 每种命中跨过多页边界；文件写入顺序刻意与排名相反。
#[test]
fn all_pages_preserve_title_priority_and_path_order_without_loss_or_duplicates() {
    let temporary = tempfile::tempdir().unwrap();
    for index in (0..33).rev() {
        // 正文命中排名较低，标题不含查询，保证跨越score边界后仍能续页。
        fs::write(
            temporary.path().join(format!("body-{index:02}.md")),
            "合成 needle 正文",
        )
        .unwrap();
    }
    for index in (0..20).rev() {
        // 标题命中不依赖正文；相同score以完整相对路径排序。
        fs::write(
            temporary.path().join(format!("needle-title-{index:02}.md")),
            "合成正文",
        )
        .unwrap();
    }
    let workspace = MarkdownWorkspace::with_root(temporary.path().to_owned());
    let session = workspace.open_session().unwrap();
    let cancelled = AtomicBool::new(false);
    let mut cursor = None;
    let mut actual = Vec::new();
    let mut pages = 0;
    loop {
        // 每次仅13+sentinel候选；总结果超过core旧Vec上限50，不可假装50后已穷尽。
        let page = session
            .search_notes_page_cancellable("needle", 13, cursor.as_ref(), &cancelled)
            .unwrap();
        assert!(page.results.len() <= 13);
        actual.extend(page.results.into_iter().map(|item| item.relative_path));
        cursor = page.next;
        pages += 1;
        if cursor.is_none() {
            // 只有真实穷尽结束，测试不能用固定页数提前丢掉尾项。
            break;
        }
        assert!(pages < 6);
    }
    let expected = (0..20)
        .map(|i| format!("needle-title-{i:02}.md"))
        .chain((0..33).map(|i| format!("body-{i:02}.md")))
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
    assert_eq!(pages, 5);
}

/// K、K+1和恰好整页的终止标志不能互相混淆。
#[test]
fn continuation_requires_an_observed_extra_match() {
    for count in [12, 13, 14, 26] {
        // 每个库独立建立查询代次，防止其他测试会话影响cursor。
        let temporary = tempfile::tempdir().unwrap();
        for index in 0..count {
            fs::write(
                temporary.path().join(format!("needle-{index:02}.md")),
                "tiny",
            )
            .unwrap();
        }
        let workspace = MarkdownWorkspace::with_root(temporary.path().to_owned());
        let session = workspace.open_session().unwrap();
        let cancelled = AtomicBool::new(false);
        let first = session
            .search_notes_page_cancellable("needle", 13, None, &cancelled)
            .unwrap();
        assert_eq!(first.results.len(), count.min(13));
        assert_eq!(first.next.is_some(), count > 13);
        if let Some(cursor) = first.next {
            // 26条的第二页恰好13，仍必须next=None；14条则只返回最后1条。
            let next = session
                .search_notes_page_cancellable("needle", 13, Some(&cursor), &cancelled)
                .unwrap();
            assert_eq!(next.results.len(), count - 13);
            assert!(next.next.is_none());
        }
    }
}

/// 错查询、跨库、旧代次与预取消均拒绝，不能重放旧页或取消另一查询。
#[test]
fn cursor_is_bound_to_query_root_and_session_generation() {
    let temporary = tempfile::tempdir().unwrap();
    for index in 0..3 {
        fs::write(
            temporary.path().join(format!("needle-{index}.md")),
            "needle",
        )
        .unwrap();
    }
    let workspace = MarkdownWorkspace::with_root(temporary.path().to_owned());
    let session = workspace.open_session().unwrap();
    let cancelled = AtomicBool::new(false);
    let first = session
        .search_notes_page_cancellable(" needle ", 1, None, &cancelled)
        .unwrap();
    let cursor = first.next.unwrap();
    assert!(matches!(
        session.search_notes_page_cancellable("different", 1, Some(&cursor), &cancelled),
        Err(MarkdownWorkspaceError::InvalidSearchCursor)
    ));
    let other_root = tempfile::tempdir().unwrap();
    let other_workspace = MarkdownWorkspace::with_root(other_root.path().to_owned());
    let other = other_workspace.open_session().unwrap();
    assert!(matches!(
        other.search_notes_page_cancellable("needle", 1, Some(&cursor), &cancelled),
        Err(MarkdownWorkspaceError::InvalidSearchCursor)
    ));
    cancelled.store(true, AtomicOrdering::Release);
    assert!(matches!(
        session.search_notes_page_cancellable("needle", 1, Some(&cursor), &cancelled),
        Err(MarkdownWorkspaceError::SearchSuperseded)
    ));
    cancelled.store(false, AtomicOrdering::Release);
    // 预取消没有取得查询权限，因此不能破坏仍有效的同一cursor。
    assert!(
        session
            .search_notes_page_cancellable("needle", 1, Some(&cursor), &cancelled)
            .is_ok()
    );
    session.cancel_search();
    assert!(matches!(
        session.search_notes_page_cancellable("needle", 1, Some(&cursor), &cancelled),
        Err(MarkdownWorkspaceError::SearchSuperseded)
    ));
    let fresh = session
        .search_notes_page_cancellable("needle", 1, None, &cancelled)
        .unwrap()
        .next
        .unwrap();
    session
        .search_notes_page_cancellable("different", 1, None, &cancelled)
        .unwrap();
    assert!(matches!(
        session.search_notes_page_cancellable("needle", 1, Some(&fresh), &cancelled),
        Err(MarkdownWorkspaceError::SearchSuperseded)
    ));
}

/// 修改与根目录重建也不能复用原查询的排名前缀。
#[test]
fn local_mutation_and_replaced_root_revoke_continuation() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("vault");
    fs::create_dir(&root).unwrap();
    for index in 0..3 {
        fs::write(root.join(format!("needle-{index}.md")), "needle").unwrap();
    }
    let workspace = MarkdownWorkspace::with_root(root.clone());
    let session = workspace.open_session().unwrap();
    let cancelled = AtomicBool::new(false);
    let cursor = session
        .search_notes_page_cancellable("needle", 1, None, &cancelled)
        .unwrap()
        .next
        .unwrap();
    session.create_note("", "needle-new").unwrap();
    assert!(matches!(
        session.search_notes_page_cancellable("needle", 1, Some(&cursor), &cancelled),
        Err(MarkdownWorkspaceError::SearchSuperseded)
    ));
    let fresh = session
        .search_notes_page_cancellable("needle", 1, None, &cancelled)
        .unwrap()
        .next
        .unwrap();
    fs::rename(&root, temporary.path().join("old-vault")).unwrap();
    fs::create_dir(&root).unwrap();
    assert!(matches!(
        session.search_notes_page_cancellable("needle", 1, Some(&fresh), &cancelled),
        Err(MarkdownWorkspaceError::InvalidSearchCursor)
    ));
}

/// Actual mmap saves revoke old rankings; saving an unchanged root preserves its current page.
#[cfg(feature = "native-markdown-candidate")]
#[test]
fn content_publications_revoke_old_cursor_but_clean_save_preserves_it() {
    let temporary = tempfile::tempdir().unwrap();
    for index in 0..3 {
        // Title matches keep all three notes eligible, so assertions isolate the actual save's authority change.
        fs::write(
            temporary.path().join(format!("needle-{index}.md")),
            "before",
        )
        .unwrap();
    }
    let workspace = MarkdownWorkspace::with_root(temporary.path().to_owned());
    let session = workspace.open_session().unwrap();
    let cancelled = AtomicBool::new(false);
    let mut document = session.open_document_buffer("needle-0.md").unwrap();
    let published = session
        .search_notes_page_cancellable("needle", 1, None, &cancelled)
        .unwrap();
    let authorization = published.authority;
    let cursor = published.next.unwrap();
    session.save_document_buffer(&mut document).unwrap();
    assert!(authorization.is_current(&session, "needle"));
    assert!(
        session
            .search_notes_page_cancellable("needle", 1, Some(&cursor), &cancelled)
            .is_ok()
    );
    document
        .replace_utf16_range_with_selection(
            0..6,
            "after",
            crate::MarkdownTextSelection::caret(0),
            crate::MarkdownTextSelection::caret(5),
        )
        .unwrap();
    session.save_document_buffer(&mut document).unwrap();
    assert!(matches!(
        session.search_notes_page_cancellable("needle", 1, Some(&cursor), &cancelled),
        Err(MarkdownWorkspaceError::SearchSuperseded)
    ));
    assert!(!authorization.is_current(&session, "needle"));
    let pending_last = session
        .search_notes_page_cancellable("needle", 13, None, &cancelled)
        .unwrap();
    assert!(pending_last.next.is_none());
    // Open the second note through the same production mmap path; the test no longer depends on
    // a legacy whole-string editor to revoke an already prepared terminal search page.
    let mut second_document = session.open_document_buffer("needle-1.md").unwrap();
    second_document
        .replace_utf16_range_with_selection(
            0..6,
            "second saved",
            crate::MarkdownTextSelection::caret(0),
            crate::MarkdownTextSelection::caret(12),
        )
        .unwrap();
    session.save_document_buffer(&mut second_document).unwrap();
    // A completed page awaiting delivery still needs revocable authority even without a next
    // cursor. The real second save must prevent installing results prepared against older bytes.
    assert!(!pending_last.authority.is_current(&session, "needle"));
}

/// 末页没有next仍须携带可撤销权限；超界查询不能消耗当前有效代次。
#[test]
fn last_page_authority_and_raw_query_budget_are_enforced_before_publication() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("needle.md"), "tiny").unwrap();
    let workspace = MarkdownWorkspace::with_root(temporary.path().to_owned());
    let session = workspace.open_session().unwrap();
    let cancelled = AtomicBool::new(false);
    let page = session
        .search_notes_page_cancellable("needle", 13, None, &cancelled)
        .unwrap();
    assert!(page.next.is_none());
    assert!(page.authority.is_current(&session, "needle"));
    for query in [
        " ".repeat(129),
        format!("{}needle", " ".repeat(128)),
        format!("needle{}", " ".repeat(128)),
    ] {
        // 原始padding超过128scalar直接拒绝，不能先trim接受或推进epoch。
        assert!(matches!(
            session.search_notes_page_cancellable(&query, 13, None, &cancelled),
            Err(MarkdownWorkspaceError::InvalidSearchCursor)
        ));
        assert!(page.authority.is_current(&session, "needle"));
    }
    assert!(!page.authority.is_current(&session, "changed query"));
    session.cancel_search();
    assert!(!page.authority.is_current(&session, "needle"));
}
