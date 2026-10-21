//! 用有限合成候选核全尾部校验、跨窗口KMP、精确摘要与路径撤权；不用大正文复制过门禁。
use super::*;

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
    // 首窗口尾切入CJK/emoji并横跨查询，前后同时含空白、控制符与组合标记。
    for padding in 4080..4110 {
        let text = format!(
            "{}{}needle中🙂{}",
            "a".repeat(padding),
            "中文🙂e\u{301}\t\r\n",
            "尾🙂\t\u{7} \n".repeat(120)
        );
        fs::write(&path, text.as_bytes()).unwrap();
        for query in ["needle中🙂", "NEEDLE", "文🙂e\u{301}", "不存在"] {
            let pattern = SearchPattern::new(query.as_bytes());
            let expected_start = pattern.find(text.as_bytes(), || true).unwrap();
            let expected =
                expected_start.map(|start| bounded_search_snippet(&text, start, query.len()));
            let actual = inspect(
                &path,
                || true,
                |source| {
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
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("candidate.md");
    let text = "原字节\r\n🙂e\u{301}".repeat(1000);
    fs::write(&path, &text).unwrap();
    let inspected = inspect_bounded_markdown(&path, MAX_SEARCH_FILE_BYTES, |_| Ok(())).unwrap();
    assert_eq!(inspected.revision, revision_for_bytes(text.as_bytes()));
    let called = std::cell::Cell::new(false);
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
    // 泛型callback只看局部或metadata也必须完整验证候选，不能发布开头合法的半结果。
    assert!(matches!(
        inspect(&path, || true, |_| Ok(7)),
        Err(MarkdownWorkspaceError::InvalidUtf8Document)
    ));
    fs::write(&path, vec![b'a'; 8192]).unwrap();
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
