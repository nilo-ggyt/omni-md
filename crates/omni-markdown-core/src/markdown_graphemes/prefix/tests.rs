//! 稳定记录和独立GB2事实分开验证；非BMP/组合字不由UTF16差值推算ordinal。
use super::*;

const ONE: MarkdownGraphemeBudget = MarkdownGraphemeBudget {
    bytes: 1,
    scalars: 1,
    records: 1,
};
const EMPTY: MarkdownGraphemeBoundary = MarkdownGraphemeBoundary {
    byte: usize::MAX,
    utf16: usize::MAX,
};

fn fixture(
    text: &str,
) -> (
    tempfile::TempDir,
    crate::MarkdownDocumentBuffer,
    MarkdownLayoutTextSource,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("真实字素前缀.md");
    std::fs::write(&path, text).unwrap();
    let document = crate::MarkdownDocumentBuffer::open(&path, text.len() as u64 + 65536).unwrap();
    let source = MarkdownLayoutTextSource::capture(&document);
    (directory, document, source)
}

#[test]
fn gb2_exists_before_scanning_but_never_creates_a_record_ordinal_or_next_cluster() {
    let (_directory, _document, source) = fixture("中文𠮷");
    let authority = source.authority();
    let mut job = MarkdownGraphemeJob::new_paged(source.clone()).unwrap();
    {
        let mut prefix = job.stable_prefix(&authority).unwrap();
        let facts = prefix.facts().unwrap();
        assert_eq!(
            (
                facts.known_boundary_count,
                facts.complete,
                facts.total_count
            ),
            (0, false, None)
        );
        assert!(facts.confirmed_through.is_none() && facts.last_committed_boundary.is_none());
        assert_eq!(
            prefix.source_end_boundary().unwrap(),
            MarkdownGraphemeBoundary { byte: 10, utf16: 4 }
        );
        let mut output = [EMPTY];
        assert_eq!(
            prefix.read_boundaries(0..1, &mut output).unwrap(),
            MarkdownGraphemePrefixRead::Pending
        );
        assert_eq!(output, [EMPTY]);
    }
    assert!(!job.is_ready());
    assert_eq!(job.storage_profile().source_bytes_read, 0);
    assert_eq!(
        job.query(
            &authority,
            MarkdownGraphemeUnit::Utf16,
            0,
            MarkdownGraphemeQuery::Next
        )
        .unwrap(),
        MarkdownGraphemeAnswer::Pending
    );
    assert_eq!(
        job.query(
            &authority,
            MarkdownGraphemeUnit::Utf16,
            4,
            MarkdownGraphemeQuery::IsBoundary
        )
        .unwrap(),
        MarkdownGraphemeAnswer::Pending
    );
}

#[test]
fn every_quota_one_prefix_matches_explicit_non_bmp_combining_oracle_and_real_eof() {
    let (_directory, _document, source) = fixture("中𠮷A\u{301}");
    let authority = source.authority();
    let expected = [
        MarkdownGraphemeBoundary { byte: 0, utf16: 0 },
        MarkdownGraphemeBoundary { byte: 3, utf16: 1 },
        MarkdownGraphemeBoundary { byte: 7, utf16: 3 },
        MarkdownGraphemeBoundary { byte: 10, utf16: 5 },
    ]; // 明确÷中÷𠮷÷A×COMBINING ACUTE÷，不把同一个DFA作为唯一oracle。
    let mut job = MarkdownGraphemeJob::new_paged(source.clone()).unwrap();
    let mut completed = false;
    for _ in 0..32 {
        let step = job.step(&authority, ONE).unwrap();
        assert!(step.bytes <= 1 && step.scalars <= 1 && step.records <= 1);
        {
            let mut prefix = job.stable_prefix(&authority).unwrap();
            let facts = prefix.facts().unwrap();
            assert!(facts.known_boundary_count <= expected.len());
            assert_eq!(facts.total_count, facts.complete.then_some(expected.len()));
            let mut batch = [EMPTY; 4];
            assert_eq!(
                prefix
                    .read_boundaries(
                        0..facts.known_boundary_count,
                        &mut batch[..facts.known_boundary_count]
                    )
                    .unwrap(),
                MarkdownGraphemePrefixRead::Read
            );
            assert_eq!(
                &batch[..facts.known_boundary_count],
                &expected[..facts.known_boundary_count]
            );
            assert_eq!(prefix.source_end_boundary().unwrap(), expected[3]);
            if facts.complete {
                assert_eq!(facts.last_committed_boundary, Some(expected[3]));
                completed = true;
            }
        }
        if completed {
            break;
        }
    }
    assert!(completed && job.is_ready());
    assert_eq!(
        job.query(
            &authority,
            MarkdownGraphemeUnit::Utf16,
            4,
            MarkdownGraphemeQuery::IsBoundary
        )
        .unwrap(),
        MarkdownGraphemeAnswer::Known(None)
    );
}

#[test]
fn giant_cluster_retains_only_the_start_record_and_pending_next_despite_known_gb2() {
    let text = format!("A{}", "\u{301}".repeat(4096));
    let (_directory, _document, source) = fixture(&text);
    let authority = source.authority();
    let mut job = MarkdownGraphemeJob::new_paged(source.clone()).unwrap();
    for _ in 0..1024 {
        job.step(&authority, ONE).unwrap();
    }
    {
        let mut prefix = job.stable_prefix(&authority).unwrap();
        let facts = prefix.facts().unwrap();
        assert_eq!(facts.known_boundary_count, 1);
        assert_eq!(facts.total_count, None);
        assert!(!facts.complete);
        assert_eq!(prefix.source_end_boundary().unwrap().utf16, 4097);
        let mut output = [EMPTY];
        assert_eq!(
            prefix.read_boundaries(1..2, &mut output).unwrap(),
            MarkdownGraphemePrefixRead::Pending
        );
        assert_eq!(output, [EMPTY]);
    }
    assert_eq!(
        job.query(
            &authority,
            MarkdownGraphemeUnit::Utf16,
            0,
            MarkdownGraphemeQuery::Next
        )
        .unwrap(),
        MarkdownGraphemeAnswer::Pending
    );
}

#[test]
fn prefix_batches_are_real_records_and_empty_source_has_one_final_boundary() {
    let (_directory, _document, source) = fixture(&"中".repeat(400));
    let authority = source.authority();
    let mut job = MarkdownGraphemeJob::new_paged(source.clone()).unwrap();
    for _ in 0..900 {
        job.step(&authority, ONE).unwrap();
    }
    let before = job.storage_profile();
    {
        let mut prefix = job.stable_prefix(&authority).unwrap();
        assert!(!prefix.facts().unwrap().complete);
        let mut output = [EMPTY; 256];
        assert_eq!(
            prefix.read_boundaries(0..256, &mut output).unwrap(),
            MarkdownGraphemePrefixRead::Read
        );
        for (ordinal, value) in output.into_iter().enumerate() {
            assert_eq!((value.byte, value.utf16), (ordinal * 3, ordinal));
        }
    }
    let after = job.storage_profile();
    assert_eq!(after.index_records_read - before.index_records_read, 256);
    assert_eq!(after.source_bytes_read, before.source_bytes_read);
    {
        let mut prefix = job.stable_prefix(&authority).unwrap();
        assert!(matches!(
            prefix.read_boundaries(0..257, &mut [EMPTY; 257]),
            Err(MarkdownGraphemeError::InvalidRange)
        ));
    }
    assert!(matches!(
        job.stable_prefix(&authority),
        Err(MarkdownGraphemeError::Poisoned)
    ));

    let (_empty_directory, _empty_document, empty) = fixture("");
    let authority = empty.authority();
    let mut job = MarkdownGraphemeJob::new_paged(empty).unwrap();
    {
        let mut prefix = job.stable_prefix(&authority).unwrap();
        assert_eq!(prefix.facts().unwrap().known_boundary_count, 0);
        assert_eq!(
            prefix.source_end_boundary().unwrap(),
            MarkdownGraphemeBoundary { byte: 0, utf16: 0 }
        );
    }
    assert_eq!(
        job.step(&authority, ONE).unwrap().status,
        MarkdownGraphemeStatus::Ready
    );
    let mut prefix = job.stable_prefix(&authority).unwrap();
    assert_eq!(prefix.facts().unwrap().total_count, Some(1));
    let mut output = [EMPTY];
    assert_eq!(
        prefix.read_boundaries(0..1, &mut output).unwrap(),
        MarkdownGraphemePrefixRead::Read
    );
    assert_eq!(output, [MarkdownGraphemeBoundary { byte: 0, utf16: 0 }]);
}

#[test]
fn borrowed_prefix_still_observes_real_source_failure_and_releases_original_fd_and_map() {
    let (_directory, document, source) = fixture("中文𠮷");
    let authority = source.authority();
    let backing = source.original_backing_observer().unwrap();
    let source_receipt = source.resource_observer();
    let mut job = MarkdownGraphemeJob::new_paged(source.clone()).unwrap();
    let resources = job.resource_observer();
    for _ in 0..3 {
        job.step(&authority, ONE).unwrap();
    }
    {
        let mut prefix = job.stable_prefix(&authority).unwrap();
        document.fail_next_original_reclaim_for_test();
        assert!(matches!(
            document.reclaim_original_read_windows_for_test(),
            Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                libc::EIO
            ))
        ));
        assert!(matches!(
            prefix.facts(),
            Err(MarkdownGraphemeError::Source(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
            ))
        ));
        assert!(matches!(
            prefix.source_end_boundary(),
            Err(MarkdownGraphemeError::Source(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
            ))
        ));
    }
    job.cancel();
    assert!(matches!(
        job.stable_prefix(&authority),
        Err(MarkdownGraphemeError::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
    drop(job);
    let closed = resources.snapshot();
    assert!(
        closed.metadata_owner_released
            && !closed.metadata_file_open
            && closed.metadata_map_bytes == 0
            && !closed.cleanup_failed
    );
    drop((source, document));
    assert!(source_receipt.payload_released());
    assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0);
}

#[test]
fn foreign_authority_and_cancel_never_reborrow_a_known_prefix() {
    let (_directory, _document, source) = fixture("中文");
    let (_foreign_directory, _foreign_document, foreign) = fixture("中文");
    let mut job = MarkdownGraphemeJob::new_paged(source.clone()).unwrap();
    assert!(matches!(
        job.stable_prefix(&foreign.authority()),
        Err(MarkdownGraphemeError::SourceMismatch)
    ));
    assert!(matches!(
        job.stable_prefix(&source.authority()),
        Err(MarkdownGraphemeError::Poisoned)
    ));
    let mut job = MarkdownGraphemeJob::new_paged(source.clone()).unwrap();
    job.cancel();
    assert!(matches!(
        job.stable_prefix(&source.authority()),
        Err(MarkdownGraphemeError::Poisoned)
    ));
}
