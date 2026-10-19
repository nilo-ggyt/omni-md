//! 开放证明与完整原SCX结果逐范围比较；有限测试镜像不进入产品状态。
use super::*;

const ONE: MarkdownScriptBudget = MarkdownScriptBudget {
    bytes: 1,
    scalars: 1,
    records: 1,
    stack_ops: 1,
};

fn fixture(
    text: &str,
) -> (
    tempfile::TempDir,
    crate::MarkdownDocumentBuffer,
    MarkdownLayoutTextSource,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("稳定脚本前缀.md");
    std::fs::write(&path, text).unwrap();
    let document = crate::MarkdownDocumentBuffer::open(&path, text.len() as u64 + 65536).unwrap();
    let source = MarkdownLayoutTextSource::capture(&document);
    (directory, document, source)
}

fn job(
    source: &MarkdownLayoutTextSource,
    cancellation: &MarkdownScriptCancellation,
) -> MarkdownScriptJob {
    MarkdownScriptJob::new_streaming(source.clone(), &source.authority(), cancellation.clone())
        .unwrap()
}

fn finish(job: &mut MarkdownScriptJob, authority: &MarkdownLayoutTextAuthority) {
    for _ in 0..1_000_000 {
        if job.step(authority, ONE).unwrap().status == MarkdownScriptStatus::Ready {
            return;
        }
    }
    panic!("有限SCX原状态机未完成");
}

#[test]
fn pure_han_and_distant_latin_offer_singleton_prefix_without_closing_the_run_or_reading_the_tail() {
    for text in ["中文".repeat(4096), format!("{}tail", "中文".repeat(4096))] {
        let (_directory, _document, source) = fixture(&text);
        let authority = source.authority();
        let cancellation = MarkdownScriptCancellation::new();
        let mut actual = job(&source, &cancellation);
        let mut observed = None;
        for _ in 0..64 {
            actual.step(&authority, ONE).unwrap();
            {
                if let MarkdownScriptPrefixAnswer::Known(Some(prefix)) = actual
                    .stable_prefix_at(&authority, MarkdownScriptUnit::Utf16, 0)
                    .unwrap()
                {
                    let facts = prefix.facts().unwrap();
                    assert_eq!(
                        prefix.kind().unwrap(),
                        MarkdownScriptPrefixKind::StableOpenPrefix
                    );
                    assert!(prefix.source_authority().unwrap() == &authority);
                    assert_eq!(
                        (
                            facts.start_byte,
                            facts.start_utf16,
                            facts.script.iso15924_tag()
                        ),
                        (0, 0, *b"Hani")
                    );
                    assert!(facts.end_utf16 > 0 && facts.end_utf16 < source.utf16_len());
                    observed = Some(facts);
                }
            }
            if observed.is_some() {
                break;
            }
        }
        let first = observed.expect("singleton前缀不需等纯Hani完整EOF");
        assert!(actual.storage_profile().source_bytes_read <= 64);
        assert_eq!(actual.storage_profile().completed_runs, 0);
        assert_eq!(
            actual
                .run_at(&authority, MarkdownScriptUnit::Utf16, 0)
                .unwrap(),
            MarkdownScriptAnswer::Pending
        );
        let mut oracle = job(&source, &cancellation);
        finish(&mut oracle, &authority);
        let MarkdownScriptAnswer::Known(Some(complete)) = oracle
            .run_at(&authority, MarkdownScriptUnit::Utf16, 0)
            .unwrap()
        else {
            panic!("原完整SCX必须有run")
        };
        assert_eq!(first.script, complete.script);
        assert!(first.end_byte <= complete.end_byte && first.end_utf16 <= complete.end_utf16);
    }
}

#[test]
fn every_published_prefix_matches_complete_original_runs_in_mixed_text_and_nested_pairs() {
    for text in [
        "中文abc中文DEF",
        "  中(abc)文 a(α)中",
        "中𠮷文(abc)尾",
        "aーあא",
        "(中(α)文)",
    ] {
        let (_directory, _document, source) = fixture(text);
        let authority = source.authority();
        let cancellation = MarkdownScriptCancellation::new();
        let mut oracle = job(&source, &cancellation);
        finish(&mut oracle, &authority);
        let mut actual = job(&source, &cancellation);
        for _ in 0..10_000 {
            let ready = actual.step(&authority, ONE).unwrap().status == MarkdownScriptStatus::Ready;
            for offset in 0..source.utf16_len() {
                if let MarkdownScriptPrefixAnswer::Known(Some(prefix)) = actual
                    .stable_prefix_at(&authority, MarkdownScriptUnit::Utf16, offset)
                    .unwrap()
                {
                    let facts = prefix.facts().unwrap();
                    let MarkdownScriptAnswer::Known(Some(expected)) = oracle
                        .run_at(&authority, MarkdownScriptUnit::Utf16, offset)
                        .unwrap()
                    else {
                        panic!("完整oracle覆盖每个原unit")
                    };
                    assert_eq!(
                        (facts.start_byte, facts.start_utf16, facts.script),
                        (expected.start_byte, expected.start_utf16, expected.script)
                    );
                    assert!(
                        facts.end_byte <= expected.end_byte
                            && facts.end_utf16 <= expected.end_utf16
                    );
                    if prefix.kind().unwrap() == MarkdownScriptPrefixKind::ClosedRun {
                        assert_eq!(facts, expected);
                    }
                }
            }
            if ready {
                break;
            }
        }
        assert_eq!(
            actual.storage_profile().completed_runs,
            oracle.storage_profile().completed_runs
        );
        assert!(matches!(
            actual
                .stable_prefix_at(&authority, MarkdownScriptUnit::Utf16, source.utf16_len())
                .unwrap(),
            MarkdownScriptPrefixAnswer::Known(None)
        ));
    }
}

#[test]
fn ambiguous_scx_common_and_unclosed_giant_cluster_stay_pending() {
    for text in [
        format!("{}カ", "ー".repeat(1024)),
        format!("{}中", "🙂".repeat(1024)),
        format!("中{}文", "\u{301}".repeat(2048)),
    ] {
        let (_directory, _document, source) = fixture(&text);
        let authority = source.authority();
        let mut actual = job(&source, &MarkdownScriptCancellation::new());
        for _ in 0..512 {
            actual.step(&authority, ONE).unwrap();
            assert!(matches!(
                actual
                    .stable_prefix_at(&authority, MarkdownScriptUnit::Utf16, 0)
                    .unwrap(),
                MarkdownScriptPrefixAnswer::Pending
            ));
        }
        assert_eq!(actual.storage_profile().completed_runs, 0);
    } // 多候选、未定Common及首个未闭合cluster都不能借primary或GB2冒充稳定脚本。
}

#[test]
fn action_in_progress_never_exposes_an_open_prefix_and_rtl_is_only_a_script_fact() {
    let text = format!("a({}α{}א)", "(".repeat(80), ")".repeat(80));
    let (_directory, _document, source) = fixture(&text);
    let authority = source.authority();
    let mut actual = job(&source, &MarkdownScriptCancellation::new());
    let mut pending_action = false;
    for _ in 0..100_000 {
        let ready = actual.step(&authority, ONE).unwrap().status == MarkdownScriptStatus::Ready;
        if actual.machine.has_pending_action() {
            pending_action = true;
            assert!(actual.machine.stable_open_prefix().is_none());
        }
        if ready {
            break;
        }
    }
    assert!(pending_action);
    let (_other_directory, _other_document, rtl_source) = fixture(&"אבג".repeat(100));
    let rtl_authority = rtl_source.authority();
    let mut rtl = job(&rtl_source, &MarkdownScriptCancellation::new());
    let mut found = false;
    for _ in 0..64 {
        rtl.step(&rtl_authority, ONE).unwrap();
        if let MarkdownScriptPrefixAnswer::Known(Some(prefix)) = rtl
            .stable_prefix_at(&rtl_authority, MarkdownScriptUnit::Utf16, 0)
            .unwrap()
        {
            assert_eq!(prefix.facts().unwrap().script.iso15924_tag(), *b"Hebr");
            found = true;
            break;
        }
    }
    assert!(found); // Hebr事实不包含level/base/方向；后继UBA不得据此跳过双向规则。
}

#[test]
fn borrowed_prefix_observes_cancel_source_errno_and_actual_owner_release() {
    for source_error in [false, true] {
        let (_directory, document, source) = fixture(&"中文".repeat(128));
        let authority = source.authority();
        let cancellation = MarkdownScriptCancellation::new();
        let backing = source.original_backing_observer().unwrap();
        let source_receipt = source.resource_observer();
        let mut actual = job(&source, &cancellation);
        let resources = actual.resource_observer();
        for _ in 0..64 {
            actual.step(&authority, ONE).unwrap();
        }
        {
            let MarkdownScriptPrefixAnswer::Known(Some(prefix)) = actual
                .stable_prefix_at(&authority, MarkdownScriptUnit::Utf16, 0)
                .unwrap()
            else {
                panic!("先取得真实稳定前缀")
            };
            if source_error {
                document.fail_next_original_reclaim_for_test();
                assert!(matches!(
                    document.reclaim_original_read_windows_for_test(),
                    Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                        libc::EIO
                    ))
                ));
            }
            cancellation.cancel();
            if source_error {
                assert!(matches!(
                    prefix.facts(),
                    Err(MarkdownScriptError::Source(
                        MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
                    ))
                ));
            } else {
                assert!(matches!(
                    prefix.facts(),
                    Err(MarkdownScriptError::Cancelled)
                ));
            }
        }
        actual.close().unwrap();
        let closed = resources.snapshot();
        assert!(
            !closed.run_file_open
                && !closed.stack_file_open
                && closed.mapped_bytes == 0
                && !closed.cleanup_failed
        );
        drop((source, document));
        assert!(source_receipt.payload_released());
        assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0);
    }
}

#[test]
fn equal_bytes_foreign_authority_cannot_borrow_the_original_prefix() {
    let (_directory, _document, source) = fixture("中文");
    let (_foreign_directory, _foreign_document, foreign) = fixture("中文");
    let mut actual = job(&source, &MarkdownScriptCancellation::new());
    assert!(matches!(
        actual.stable_prefix_at(&foreign.authority(), MarkdownScriptUnit::Utf16, 0),
        Err(MarkdownScriptError::SourceMismatch)
    ));
    assert!(matches!(
        actual.stable_prefix_at(&source.authority(), MarkdownScriptUnit::Utf16, 0),
        Err(MarkdownScriptError::Poisoned)
    ));
}
