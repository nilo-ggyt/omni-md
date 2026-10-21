//! Compare exact whole-source printable proof with the original Unicode machine; Dense's own
//! integer output is not the sole oracle.
use super::*;

fn dense_profile(job: &MarkdownGraphemeJob, bytes: usize) {
    // Exact source proof can certify logical boundaries without text reads or physical
    // metadata rows. These owned counters do not measure whole-process resident memory.
    let profile = job.storage_profile();
    assert!(profile.dense_printable_ascii);
    assert_eq!(
        (
            profile.source_bytes,
            profile.source_utf16,
            profile.boundary_count
        ),
        (bytes, bytes, bytes + 1)
    );
    assert_eq!(
        (
            profile.source_bytes_read,
            profile.source_borrow_max_bytes,
            profile.boundary_records
        ),
        (0, 0, 0)
    );
    assert_eq!(
        (
            profile.index_records_read,
            profile.index_records_written,
            profile.index_page_loads,
            profile.index_peak_mapped_bytes
        ),
        (0, 0, 0, 0)
    );
}

#[test]
fn exact_printable_domain_and_empty_compare_all_queries_with_original_paged_machine() {
    // Empty input and the complete printable-ASCII domain must agree with the actual
    // paged Unicode machine for strict neighbors and membership at every coordinate.
    for bytes in [Vec::new(), (0x20u8..=0x7e).collect::<Vec<_>>()] {
        let original = source(&bytes);
        let authority = original.authority();
        let mut dense = MarkdownGraphemeJob::new(original.clone()).unwrap();
        let mut paged = MarkdownGraphemeJob::new_paged(original.clone()).unwrap();
        finish(&mut paged, &authority);
        assert!(dense.is_ready());
        dense_profile(&dense, bytes.len());
        let step = dense.step(&authority, TINY).unwrap();
        assert_eq!(step.status, MarkdownGraphemeStatus::Ready);
        assert_eq!((step.bytes, step.scalars, step.records), (0, 0, 0));
        for unit in [MarkdownGraphemeUnit::Byte, MarkdownGraphemeUnit::Utf16] {
            for offset in 0..=bytes.len() {
                for query in [
                    MarkdownGraphemeQuery::Previous,
                    MarkdownGraphemeQuery::Next,
                    MarkdownGraphemeQuery::IsBoundary,
                ] {
                    assert_eq!(
                        dense.query(&authority, unit, offset, query).unwrap(),
                        paged.query(&authority, unit, offset, query).unwrap()
                    );
                }
            }
        }
        dense_profile(&dense, bytes.len());
        assert_eq!(paged.storage_profile().boundary_records, bytes.len() + 1);
        assert!(paged.storage_profile().index_records_read > 0);
        assert_eq!(
            paged.storage_profile().index_records_written,
            (bytes.len() + 1) as u64
        );
        // Dense never allocates paged storage; its still-live owner is nevertheless not
        // released until actual close, independently of Paged's real cleanup receipt.
        let dense_resources = dense.resource_observer();
        let paged_resources = paged.resource_observer();
        assert!(!dense_resources.snapshot().metadata_storage_allocated);
        assert!(!dense_resources.snapshot().metadata_owner_released);
        assert!(paged_resources.snapshot().metadata_storage_allocated);
        dense.close().unwrap();
        paged.close().unwrap();
        assert!(dense_resources.snapshot().metadata_owner_released);
        assert!(paged_resources.snapshot().metadata_owner_released);
    }
}

#[test]
fn three_lengths_export_global_256_batches_without_source_or_record_io_and_release_actual_owners() {
    // Different original lengths retain exact global coordinates in capped batches,
    // including the final 256-record window near actual EOF.
    for length in [4096, 8192, 32768] {
        let original = source(&b"A".repeat(length));
        let authority = original.authority();
        let backing = original.original_backing_observer().unwrap();
        let mut job = MarkdownGraphemeJob::new(original.clone()).unwrap();
        let observer = job.resource_observer();
        assert_eq!(
            job.ready_boundary_count(&authority).unwrap(),
            Some(length + 1)
        );
        let sentinel = MarkdownGraphemeBoundary {
            byte: usize::MAX,
            utf16: usize::MAX,
        };
        for start in [0, 17, length - 255] {
            let mut batch = [sentinel; 256];
            assert!(
                job.read_ready_boundaries(&authority, start..start + 256, &mut batch)
                    .unwrap()
            );
            for (ordinal, actual) in batch.into_iter().enumerate() {
                let global = start + ordinal;
                assert_eq!(
                    actual,
                    MarkdownGraphemeBoundary {
                        byte: global,
                        utf16: global
                    }
                );
            }
        }
        assert!(matches!(
            job.query(
                &authority,
                MarkdownGraphemeUnit::Utf16,
                length,
                MarkdownGraphemeQuery::Next
            )
            .unwrap(),
            MarkdownGraphemeAnswer::Known(None)
        ));
        dense_profile(&job, length);
        // Zero physical allocation is not a successful close. Keep the live Dense owner
        // distinct from the external source clone that still retains the original mapping.
        let before = observer.snapshot();
        assert!(
            !before.metadata_storage_allocated
                && !before.metadata_file_open
                && !before.metadata_owner_released
                && before.metadata_map_bytes == 0
                && before.metadata_buffer_bytes == 0
                && before.metadata_peak_buffer_bytes == 0
        );
        job.close().unwrap();
        let closed = observer.snapshot();
        assert!(closed.metadata_owner_released && !closed.metadata_storage_allocated);
        assert!(!closed.source_payload_released && !closed.cleanup_failed);
        drop(original);
        assert!(observer.snapshot().source_payload_released);
        assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0);
    }
}

#[test]
fn controls_unicode_and_unread_tail_are_not_admitted_from_an_ascii_prefix_or_equal_lengths() {
    // A printable prefix, equal coordinate lengths, or familiar ASCII surroundings do
    // not prove the whole source domain when controls or any Unicode scalar remain.
    for text in [
        "A\tB",
        "A\nB",
        "A\r\nB",
        "A\u{7f}B",
        "AéB",
        "A中B",
        "Aa\u{301}B",
        "A\u{200d}B",
        "A\u{ad}B",
        "A😀B",
    ] {
        let original = source(text.as_bytes());
        let authority = original.authority();
        let mut job = MarkdownGraphemeJob::new(original).unwrap();
        assert!(!job.storage_profile().dense_printable_ascii);
        let observer = job.resource_observer();
        assert!(
            observer.snapshot().metadata_storage_allocated
                && !observer.snapshot().metadata_file_open
                && !observer.snapshot().metadata_owner_released
        );
        assert!(!job.is_ready());
        assert!(matches!(
            job.query(
                &authority,
                MarkdownGraphemeUnit::Utf16,
                0,
                MarkdownGraphemeQuery::IsBoundary
            )
            .unwrap(),
            MarkdownGraphemeAnswer::Pending
        ));
        finish(&mut job, &authority);
        assert!(job.storage_profile().boundary_records > 0);
        job.close().unwrap();
        assert!(observer.snapshot().metadata_owner_released);
    }
}

#[test]
fn foreign_authority_cancel_and_invalid_batch_revoke_dense_without_fabricating_close() {
    // Authority mismatch, cancellation, and oversized export revoke logical use while
    // leaving the real Dense owner live until Drop; no storage release may be invented.
    for mode in 0..3 {
        let original = source(b"printable");
        let authority = original.authority();
        let mut job = MarkdownGraphemeJob::new(original).unwrap();
        let observer = job.resource_observer();
        match mode {
            0 => {
                let foreign = source(b"printable");
                assert!(matches!(
                    job.query(
                        &foreign.authority(),
                        MarkdownGraphemeUnit::Utf16,
                        0,
                        MarkdownGraphemeQuery::IsBoundary
                    ),
                    Err(MarkdownGraphemeError::SourceMismatch)
                ));
            }
            1 => job.cancel(),
            _ => {
                let sentinel = MarkdownGraphemeBoundary {
                    byte: usize::MAX,
                    utf16: usize::MAX,
                };
                let mut batch = [sentinel; 257];
                assert!(matches!(
                    job.read_ready_boundaries(&authority, 0..257, &mut batch),
                    Err(MarkdownGraphemeError::InvalidRange)
                ));
                assert!(batch.iter().all(|value| *value == sentinel));
            }
        }
        assert!(matches!(
            job.step(&authority, TINY),
            Err(MarkdownGraphemeError::Poisoned)
        ));
        assert!(!observer.snapshot().metadata_owner_released);
        drop(job);
        let closed = observer.snapshot();
        assert!(
            !closed.metadata_storage_allocated
                && closed.metadata_owner_released
                && closed.source_payload_released
                && !closed.cleanup_failed
        );
    }
}

#[test]
fn original_eio_precedes_cancel_ready_and_dense_arithmetic_then_close_returns_real_first_error() {
    // Whole-source proof grants no exemption from original Source health. Actual reclaim
    // EIO precedes old Ready, cancellation, arithmetic queries, and later construction.
    let (_directory, document) = fixture(b"printable");
    let original = MarkdownLayoutTextSource::capture(&document);
    let authority = original.authority();
    let backing = original.original_backing_observer().unwrap();
    let mut job = MarkdownGraphemeJob::new(original.clone()).unwrap();
    let observer = job.resource_observer();
    assert!(job.is_ready());
    poison_original_read_windows(&document);
    job.cancel();
    for _ in 0..2 {
        assert!(matches!(
            job.query(
                &authority,
                MarkdownGraphemeUnit::Utf16,
                0,
                MarkdownGraphemeQuery::IsBoundary
            ),
            Err(MarkdownGraphemeError::Source(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
            ))
        ));
    }
    assert!(matches!(
        job.step(&authority, TINY),
        Err(MarkdownGraphemeError::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
    assert!(matches!(
        MarkdownGraphemeJob::new(original.clone()),
        Err(MarkdownGraphemeError::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
    assert!(!observer.snapshot().metadata_owner_released);
    // Explicit close still retires the Dense owner before returning the real Source
    // failure; the last independent source/document owners control backing unmap.
    assert!(matches!(
        job.close(),
        Err(MarkdownGraphemeError::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
    assert!(observer.snapshot().metadata_owner_released);
    drop((original, document));
    assert!(observer.snapshot().source_payload_released && backing.unmap_succeeded());
}

#[test]
fn original_frozen_dense_stays_exact_across_edit_and_undo_without_owning_document_history() {
    // Editing changes the current source domain; undo creates another capture rather
    // than restoring the old authority. The original immutable Dense source stays exact.
    let (_directory, mut document) = fixture(b"ABC");
    let original = MarkdownLayoutTextSource::capture(&document);
    let authority = original.authority();
    let mut job = MarkdownGraphemeJob::new(original.clone()).unwrap();
    let observer = job.resource_observer();
    document
        .insert_utf16(1, "\u{301}", MarkdownTextSelection::caret(2))
        .unwrap();
    let edited = MarkdownLayoutTextSource::capture(&document);
    let other = MarkdownGraphemeJob::new(edited.clone()).unwrap();
    assert!(!other.storage_profile().dense_printable_ascii);
    document.undo().unwrap();
    let restored = MarkdownLayoutTextSource::capture(&document);
    assert!(!restored.permits(&authority));
    assert!(
        MarkdownGraphemeJob::new(restored.clone())
            .unwrap()
            .storage_profile()
            .dense_printable_ascii
    );
    // Destroy the actual document owner, then query the old frozen payload without
    // retaining its history or granting any save authority to the layout job.
    let document = Rc::new(document);
    let document_weak = Rc::downgrade(&document);
    drop(document);
    assert!(document_weak.upgrade().is_none());
    assert!(matches!(
        job.query(
            &authority,
            MarkdownGraphemeUnit::Utf16,
            3,
            MarkdownGraphemeQuery::IsBoundary
        )
        .unwrap(),
        MarkdownGraphemeAnswer::Known(Some(MarkdownGraphemeBoundary { byte: 3, utf16: 3 }))
    ));
    drop((other, edited, restored, original));
    job.close().unwrap();
    assert!(
        observer.snapshot().metadata_owner_released && observer.snapshot().source_payload_released
    );
}
