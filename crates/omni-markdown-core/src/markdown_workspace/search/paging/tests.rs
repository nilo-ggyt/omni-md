//! Verify continuation and cancellation through real isolated temporary vaults.
use super::*;

/// Cross several pages of each match class with creation order deliberately
/// reversed from the public title-first/path ordering.
#[test]
fn all_pages_preserve_title_priority_and_path_order_without_loss_or_duplicates() {
    let temporary = tempfile::tempdir().unwrap();
    for index in (0..33).rev() {
        // Body-only hits rank lower, ensuring continuation must cross the score
        // boundary rather than merely paginate title matches.
        fs::write(
            temporary.path().join(format!("body-{index:02}.md")),
            "合成 needle 正文",
        )
        .unwrap();
    }
    for index in (0..20).rev() {
        // Title hits need no body match; their full relative paths break equal scores.
        fs::write(
            temporary.path().join(format!("needle-title-{index:02}.md")),
            "合成正文",
        )
        .unwrap();
    }
    let workspace = MarkdownWorkspace::new(temporary.path().to_owned());
    let session = workspace.open_session().unwrap();
    let cancelled = AtomicBool::new(false);
    let mut cursor = None;
    let mut actual = Vec::new();
    let mut pages = 0;
    loop {
        // Retain only 13 plus a sentinel per page. The total exceeds the older
        // complete-vector cap, so reaching 50 cannot masquerade as exhaustion.
        let page = session
            .search_notes_page_cancellable("needle", 13, cursor.as_ref(), &cancelled)
            .unwrap();
        assert!(page.results.len() <= 13);
        actual.extend(page.results.into_iter().map(|item| item.relative_path));
        cursor = page.next;
        pages += 1;
        if cursor.is_none() {
            // Stop on actual exhaustion, not a fixed iteration count that could
            // silently drop valid trailing results.
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

/// Distinguish K, K+1, and an exact multiple of K when proving termination.
#[test]
fn continuation_requires_an_observed_extra_match() {
    for count in [12, 13, 14, 26] {
        // Each real vault owns independent query generations and cursor authority.
        let temporary = tempfile::tempdir().unwrap();
        for index in 0..count {
            fs::write(
                temporary.path().join(format!("needle-{index:02}.md")),
                "tiny",
            )
            .unwrap();
        }
        let workspace = MarkdownWorkspace::new(temporary.path().to_owned());
        let session = workspace.open_session().unwrap();
        let cancelled = AtomicBool::new(false);
        let first = session
            .search_notes_page_cancellable("needle", 13, None, &cancelled)
            .unwrap();
        assert_eq!(first.results.len(), count.min(13));
        assert_eq!(first.next.is_some(), count > 13);
        if let Some(cursor) = first.next {
            // The second page of 26 is exactly full yet terminal; 14 returns
            // just its one actual trailing match.
            let next = session
                .search_notes_page_cancellable("needle", 13, Some(&cursor), &cancelled)
                .unwrap();
            assert_eq!(next.results.len(), count - 13);
            assert!(next.next.is_none());
        }
    }
}

/// Reject changed queries, cross-vault credentials, old generations, and
/// pre-cancelled work without replaying pages or revoking another query.
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
    let workspace = MarkdownWorkspace::new(temporary.path().to_owned());
    let session = workspace.open_session().unwrap();
    let cancelled = AtomicBool::new(false);
    let first = session
        .search_notes_page_cancellable(" needle ", 1, None, &cancelled)
        .unwrap();
    let cursor = first.next.unwrap();
    // Query and root/session identity are independent continuation preconditions.
    assert!(matches!(
        session.search_notes_page_cancellable("different", 1, Some(&cursor), &cancelled),
        Err(MarkdownWorkspaceError::InvalidSearchCursor)
    ));
    let other_root = tempfile::tempdir().unwrap();
    let other_workspace = MarkdownWorkspace::new(other_root.path().to_owned());
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
    // Pre-cancellation acquired no query authority and therefore must preserve
    // this still-current cursor when the caller resets its own token.
    assert!(
        session
            .search_notes_page_cancellable("needle", 1, Some(&cursor), &cancelled)
            .is_ok()
    );
    session.cancel_search();
    // Explicit session cancellation permanently revokes the old credentials.
    assert!(matches!(
        session.search_notes_page_cancellable("needle", 1, Some(&cursor), &cancelled),
        Err(MarkdownWorkspaceError::SearchSuperseded)
    ));
    let fresh = session
        .search_notes_page_cancellable("needle", 1, None, &cancelled)
        .unwrap()
        .next
        .unwrap();
    // A genuinely new query supersedes the fresh generation even when the old
    // cursor's root and query text still match its own original values.
    session
        .search_notes_page_cancellable("different", 1, None, &cancelled)
        .unwrap();
    assert!(matches!(
        session.search_notes_page_cancellable("needle", 1, Some(&fresh), &cancelled),
        Err(MarkdownWorkspaceError::SearchSuperseded)
    ));
}

/// Local mutations and a recreated root cannot reuse the previous query's rank prefix.
#[test]
fn local_mutation_and_replaced_root_revoke_continuation() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("vault");
    fs::create_dir(&root).unwrap();
    for index in 0..3 {
        fs::write(root.join(format!("needle-{index}.md")), "needle").unwrap();
    }
    let workspace = MarkdownWorkspace::new(root.clone());
    let session = workspace.open_session().unwrap();
    let cancelled = AtomicBool::new(false);
    let cursor = session
        .search_notes_page_cancellable("needle", 1, None, &cancelled)
        .unwrap()
        .next
        .unwrap();
    session.create_note("", "needle-new").unwrap();
    // Use an actual public creation to revoke the original collection's query.
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
    // Reusing the root pathname with a different real inode must invalidate
    // continuation independently of its matching string path.
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
    let workspace = MarkdownWorkspace::new(temporary.path().to_owned());
    let session = workspace.open_session().unwrap();
    let cancelled = AtomicBool::new(false);
    let mut document = session.open_document_buffer("needle-0.md").unwrap();
    let published = session
        .search_notes_page_cancellable("needle", 1, None, &cancelled)
        .unwrap();
    let authorization = published.authority;
    let cursor = published.next.unwrap();
    // A clean save changes no text-derived ranking and preserves current credentials.
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
    // Actual canonical editing and mmap persistence revoke the older query.
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

/// A terminal page still needs revocable authority, while rejected raw queries
/// must not consume the currently valid generation.
#[test]
fn last_page_authority_and_raw_query_budget_are_enforced_before_publication() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("needle.md"), "tiny").unwrap();
    let workspace = MarkdownWorkspace::new(temporary.path().to_owned());
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
        // Reject padding beyond the 128-scalar raw limit before trim can accept
        // the query or allocate a new generation.
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
