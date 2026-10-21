//! Use finite synthetic files to verify tail validation, cross-lease KMP,
//! exact snippets/revisions, and path-identity revocation through real storage.
use super::*;

/// Exercise the real frozen inspection owner and return only its validated result.
fn inspect<T>(
    path: &Path,
    current: impl Fn() -> bool,
    callback: impl FnOnce(&BoundedMarkdownText) -> Result<T, MarkdownWorkspaceError>,
) -> Result<T, MarkdownWorkspaceError> {
    inspect_bounded_markdown_text_cancellable(path, MAX_SEARCH_FILE_BYTES, current, callback, None)
        .map(|result| result.value)
}

#[test]
fn early_match_still_rejects_invalid_or_incomplete_utf8_at_the_actual_tail() {
    // Place a useful hit in the first lease and malformed or unfinished UTF-8
    // beyond several later leases; neither file can publish that partial success.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("candidate.md");
    let pattern = SearchPattern::new(b"needle");
    for suffix in [&[0xff][..], &[0xf0, 0x90, 0x80][..]] {
        let mut data = b"needle".to_vec();
        data.resize(3 * 4096 + 1, b'x');
        data.extend_from_slice(suffix);
        fs::write(&path, data).unwrap();
        assert!(matches!(
            inspect(&path, || true, |text| pattern.find_bounded(text, &|| true)),
            Err(MarkdownWorkspaceError::InvalidUtf8Document)
        ));
    }
}

#[test]
fn boundary_crossing_match_and_normalized_snippet_equal_the_original_whole_text_oracle() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("candidate.md");
    // Shift CJK/emoji and the query across the first lease boundary while context
    // includes whitespace, a control character, and a combining mark.
    for padding in 4080..4110 {
        let text = format!(
            "{}{}needle中🙂{}",
            "a".repeat(padding),
            "中文🙂e\u{301}\t\r\n",
            "尾🙂\t\u{7} \n".repeat(120)
        );
        fs::write(&path, text.as_bytes()).unwrap();
        for query in ["needle中🙂", "NEEDLE", "文🙂e\u{301}", "不存在"] {
            // The finite fixture's contiguous oracle isolates the externally
            // visible result from the production source's window partitioning.
            let pattern = SearchPattern::new(query.as_bytes());
            let expected_start = pattern.find(text.as_bytes(), || true).unwrap();
            let expected =
                expected_start.map(|start| bounded_search_snippet(&text, start, query.len()));
            let actual = inspect(
                &path,
                || true,
                |source| {
                    // Match and obtain context through the actual finite source
                    // API; the callback never receives a whole-document slice.
                    pattern
                        .find_bounded(source, &|| true)?
                        .map(|start| {
                            bounded_file_search_snippet(source, start, query.len(), &|| true)
                        })
                        .transpose()
                },
            )
            .unwrap();
            assert_eq!(actual, expected, "padding={padding} query={query}");
        }
    }
}

#[test]
fn all_valid_utf8_windows_are_bounded_and_cancelled_callbacks_cannot_publish() {
    // A real file spanning several leases must deliver each exact byte once
    // without offering more than the source's 4 KiB at any callback.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("candidate.md");
    let text = "中文🙂a\u{301}".repeat(1200);
    fs::write(&path, text.as_bytes()).unwrap();
    let mut visited = 0;
    inspect(
        &path,
        || true,
        |source| {
            source.visit_validated_bytes(&|| true, |start, bytes| {
                assert_eq!(start, visited);
                assert!(bytes.len() <= 4096);
                visited += bytes.len();
                Ok(())
            })
        },
    )
    .unwrap();
    assert_eq!(visited, text.len());
    let cancelled = AtomicBool::new(false);
    // Revoke the actual request from its first consumer callback. Later cleanup
    // cannot restore authority or turn the partial traversal into a successful result.
    let current = || !cancelled.load(AtomicOrdering::Acquire);
    let result = inspect(&path, current, |source| {
        source.visit_validated_bytes(&current, |_, _| {
            cancelled.store(true, AtomicOrdering::Release);
            Ok(())
        })
    });
    assert!(matches!(
        result,
        Err(MarkdownWorkspaceError::SearchSuperseded)
    ));
}

#[test]
fn post_callback_identity_change_rejects_results_and_empty_source_is_valid() {
    // Replace the file's real bytes after frozen inspection but before final
    // descriptor/path validation; derived success must be discarded.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("candidate.md");
    fs::write(&path, b"original").unwrap();
    let result = inspect(
        &path,
        || true,
        |source| {
            source.visit_validated_bytes(&|| true, |_, _| Ok(()))?;
            fs::write(&path, b"modified").unwrap();
            Ok(7)
        },
    );
    assert!(matches!(
        result,
        Err(MarkdownWorkspaceError::DocumentChangedDuringRead)
    ));
    fs::write(&path, b"").unwrap();
    // An actual empty file has a valid zero extent, rather than representing
    // a missing backing for nonempty content.
    assert_eq!(
        inspect(
            &path,
            || true,
            |source| {
                assert_eq!(source.len(), 0);
                SearchPattern::new(b"query").find_bounded(source, &|| true)
            }
        )
        .unwrap(),
        None
    );
}

#[test]
fn finite_revision_matches_original_hash_and_budget_rejects_before_callback() {
    // Hash original CR/LF and Unicode bytes exactly; presentation normalization
    // must not influence the persisted revision.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("candidate.md");
    let text = "原字节\r\n🙂e\u{301}".repeat(1000);
    fs::write(&path, &text).unwrap();
    let inspected = inspect_bounded_markdown(&path, MAX_SEARCH_FILE_BYTES, |_| Ok(())).unwrap();
    assert_eq!(inspected.revision, revision_for_bytes(text.as_bytes()));
    let called = std::cell::Cell::new(false);
    // A real extent exceeding the caller's limit fails before consumer work,
    // rather than acquiring a large source and discovering its size afterward.
    let result = inspect_bounded_markdown_text_cancellable(
        &path,
        3,
        || true,
        |_| {
            called.set(true);
            Ok(())
        },
        None,
    );
    assert!(matches!(
        result,
        Err(MarkdownWorkspaceError::DocumentTooLarge)
    ));
    assert!(!called.get());
}

#[test]
fn local_inspector_cannot_bypass_complete_utf8_confirmation_or_export_a_whole_slice() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("candidate.md");
    fs::write(&path, [b'a', 0xff]).unwrap();
    // A metadata-only consumer still owes complete candidate UTF-8 validity;
    // returning a useful scalar cannot bless an invalid unread tail.
    assert!(matches!(
        inspect(&path, || true, |_| Ok(7)),
        Err(MarkdownWorkspaceError::InvalidUtf8Document)
    ));
    fs::write(&path, vec![b'a'; 8192]).unwrap();
    // Even a valid source rejects a direct request one byte above its lease
    // limit; the general inspector cannot export a whole-source borrow.
    let result = inspect(
        &path,
        || true,
        |source| source.with_bytes(0..4097, &|| true, |_| ()),
    );
    assert!(matches!(
        result,
        Err(MarkdownWorkspaceError::StorageUnavailable)
    ));
}
