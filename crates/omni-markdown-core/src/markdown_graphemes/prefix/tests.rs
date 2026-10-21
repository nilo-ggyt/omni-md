//! Verify stable records separately from the independent GB2 endpoint; UTF-16 differences do
//! not determine ordinals for non-BMP scalars or combining sequences.
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
    // Own an isolated actual file and document, then capture their immutable layout source.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("真实字素前缀.md");
    std::fs::write(&path, text).unwrap();
    let document = crate::MarkdownDocumentBuffer::open(&path, text.len() as u64 + 65536).unwrap();
    let source = MarkdownLayoutTextSource::capture(&document);
    (directory, document, source)
}

#[test]
fn gb2_exists_before_scanning_but_never_creates_a_record_ordinal_or_next_cluster() {
    // Immutable source extent proves its GB2 coordinate before any paged records exist.
    // That fact must leave unknown records and strict neighbors pending and output unchanged.
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
fn paged_mixed_unicode_queries_keep_strict_neighbors_and_pending_prefix() {
    // The literal declares breaks around the CJK scalar, non-BMP scalar, and A plus acute:
    // each group has 10 bytes and 5 UTF-16 units. Two finite lengths leave partial/full
    // final pages. Expectations come from those explicit rules, not the index under test.
    for groups in [400, 1365] {
        let text = "中𠮷A\u{301}".repeat(groups);
        let (directory, document, source) = fixture(&text);
        let authority = source.authority();
        let source_receipt = source.resource_observer();
        let backing = source.original_backing_observer().unwrap();
        let mut job = MarkdownGraphemeJob::new_paged(source.clone()).unwrap();
        let resources = job.resource_observer();
        let quantum = MarkdownGraphemeBudget {
            bytes: 256,
            scalars: 256,
            records: 256,
        };
        assert_eq!(
            job.query(
                &authority,
                MarkdownGraphemeUnit::Utf16,
                0,
                MarkdownGraphemeQuery::Next,
            )
            .unwrap(),
            MarkdownGraphemeAnswer::Pending,
        ); // An empty append range before scanning cannot certify the first complete grapheme.
        for _ in 0..14 {
            job.step(&authority, quantum).unwrap();
        }
        assert!(!job.is_ready());
        assert!(resources.snapshot().metadata_file_open); // A real page transition acquired the FD while the tail remains unknown.
        for (unit, offset) in [
            (MarkdownGraphemeUnit::Byte, text.len()),
            (MarkdownGraphemeUnit::Utf16, groups * 5),
        ] {
            for query in [
                MarkdownGraphemeQuery::IsBoundary,
                MarkdownGraphemeQuery::Previous,
            ] {
                assert_eq!(
                    job.query(&authority, unit, offset, query).unwrap(),
                    MarkdownGraphemeAnswer::Pending,
                ); // Known source length cannot classify the unscanned tail or its strict predecessor.
            }
        }
        let mut expected = Vec::with_capacity(groups * 3 + 1);
        for block in 0..groups {
            for (byte, utf16) in [(0, 0), (3, 1), (7, 3)] {
                expected.push(MarkdownGraphemeBoundary {
                    byte: block * 10 + byte,
                    utf16: block * 5 + utf16,
                });
            }
        }
        expected.push(MarkdownGraphemeBoundary {
            byte: groups * 10,
            utf16: groups * 5,
        });
        for complete in [false, true] {
            if complete {
                let mut completed = false;
                for _ in 0..text.len() {
                    if job.step(&authority, quantum).unwrap().status
                        == MarkdownGraphemeStatus::Ready
                    {
                        completed = true;
                        break;
                    }
                }
                assert!(completed);
            }
            for (unit, stride) in [
                (MarkdownGraphemeUnit::Byte, 10),
                (MarkdownGraphemeUnit::Utf16, 5),
            ] {
                let position = |boundary: MarkdownGraphemeBoundary| match unit {
                    MarkdownGraphemeUnit::Byte => boundary.byte,
                    MarkdownGraphemeUnit::Utf16 => boundary.utf16,
                };
                // The classified prefix ends before 3500 bytes; after sealing also inspect the
                // far tail and EOF. Both phases query page starts, preceding records, and
                // interior coordinates so strict neighbors cannot be replaced by the page head.
                for block in [0, 170, 341, 349, 1023, 1024, groups - 1]
                    .into_iter()
                    .filter(|&block| block < groups && (complete || block <= 349))
                {
                    for offset in block * stride..=block * stride + stride {
                        for query in [
                            MarkdownGraphemeQuery::IsBoundary,
                            MarkdownGraphemeQuery::Previous,
                            MarkdownGraphemeQuery::Next,
                        ] {
                            let wanted = match query {
                                MarkdownGraphemeQuery::IsBoundary => expected
                                    .iter()
                                    .copied()
                                    .find(|boundary| position(*boundary) == offset),
                                MarkdownGraphemeQuery::Previous => expected
                                    .iter()
                                    .copied()
                                    .rev()
                                    .find(|boundary| position(*boundary) < offset),
                                MarkdownGraphemeQuery::Next => expected
                                    .iter()
                                    .copied()
                                    .find(|boundary| position(*boundary) > offset),
                            };
                            assert_eq!(
                                job.query(&authority, unit, offset, query).unwrap(),
                                MarkdownGraphemeAnswer::Known(wanted),
                                "groups={groups} complete={complete} unit={unit:?} offset={offset} query={query:?}",
                            );
                        }
                    }
                }
            }
        }
        // Queries leave the original document and file unchanged. This disk comparison
        // concerns only the explicitly bounded, isolated fixture created above.
        assert_eq!(document.generation(), 0);
        assert_eq!(
            std::fs::read(directory.path().join("真实字素前缀.md")).unwrap(),
            text.as_bytes()
        );
        // Close the real index first, then release the remaining source/document owners
        // before checking actual original-backing unmap completion.
        job.close().unwrap();
        let closed = resources.snapshot();
        assert!(
            closed.metadata_owner_released
                && !closed.metadata_file_open
                && closed.metadata_map_bytes == 0
                && closed.metadata_buffer_bytes == 0
                && !closed.cleanup_failed
        );
        drop((source, document));
        assert!(source_receipt.payload_released());
        assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0);
    }
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
    ]; // Explicit CJK/non-BMP/A-plus-acute boundaries are independent of the DFA under test.
    let mut job = MarkdownGraphemeJob::new_paged(source.clone()).unwrap();
    let mut completed = false;
    // Every one-unit quantum may leave UTF-8 or rule state incomplete, but the stable
    // prefix must contain only the already committed leading portion of this oracle.
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
    // The source endpoint is known independently, yet this long combining cluster has
    // no committed successor until the original machine reaches its actual end.
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
    // A prefix read consumes actual original records without scanning more text. The
    // public 256-record cap rejects a larger batch and permanently revokes this job.
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

    // Empty Paged input initially has GB2 coordinates but no record; only an actual
    // finish publishes its one shared BOF/EOF boundary and complete total.
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
    // A live prefix borrow retains the original authority checks. Reclaim failure is
    // visible through both cached facts and GB2, and takes precedence over cancellation.
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
    // Releasing the prefix and job returns metadata independently of the external source
    // owners; only dropping those last owners completes the original source unmap.
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
    // Equal text in another capture cannot authorize this prefix, and valid original
    // authority cannot revive a job already revoked by mismatch or cancellation.
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
