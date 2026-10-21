//! Exact whole-source ASCII certificate semantics, read budgets, and original resource receipts.
//! Finite complete UBA oracles stay outside the production path.

use super::*;
use crate::{
    MarkdownDocumentBuffer, MarkdownLayoutTextAuthority, MarkdownLayoutTextSource,
    MarkdownTextSelection,
};
use std::{
    future::Future,
    io::Write,
    task::{Context, Poll, Waker},
};
use unicode_bidi::utf16::BidiInfo;

const ONE: MarkdownBidiPrepareBudget = MarkdownBidiPrepareBudget {
    bytes: 1,
    scalars: 1,
    metadata_ops: 1,
};

struct Fixture {
    _directory: tempfile::TempDir,
    document: MarkdownDocumentBuffer,
}

impl Fixture {
    fn new(text: &str) -> Self {
        // The fixture owns its actual isolated file and document; captures below retain
        // only their original immutable payload, not the active document or its history.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("printable.md");
        std::fs::write(&path, text).unwrap();
        Self {
            document: MarkdownDocumentBuffer::open(&path, 8 * 1024 * 1024).unwrap(),
            _directory: directory,
        }
    }

    fn source(&self) -> MarkdownLayoutTextSource {
        MarkdownLayoutTextSource::capture(&self.document)
    }
}

fn job(
    source: MarkdownLayoutTextSource,
    base: BidiBaseDirection,
    cancel: &MarkdownBidiCancellation,
) -> (
    MarkdownLayoutTextAuthority,
    MarkdownBidiPrepareJob,
    MarkdownBidiResourceObserver,
) {
    // Obtain authority from the original payload before moving Source into the Future;
    // another capture of equal text cannot supply replacement authority.
    let authority = source.authority();
    let (result, receipt) = MarkdownBidiPrepareJob::new_layout_source_observed(
        source,
        &authority,
        base,
        BidiMemoryBudget::new(65536).unwrap(),
        cancel,
    );
    (authority, result.unwrap(), receipt)
}

fn assert_quantum(step: MarkdownBidiPrepareStep) {
    // Public work dimensions are independently capped, and each metadata operation
    // accounts for actual record or resource work rather than elapsed time or RSS.
    assert!(step.bytes <= ONE.bytes);
    assert!(step.scalars <= ONE.scalars);
    assert!(step.metadata_ops <= ONE.metadata_ops);
    assert_eq!(
        step.metadata_ops,
        step.record_reads + step.record_writes + step.resource_ops
    );
    assert!(step.source_borrow_max_bytes <= ONE.bytes);
}

fn fast_ready(
    job: &mut MarkdownBidiPrepareJob,
    authority: &MarkdownLayoutTextAuthority,
) -> MarkdownBidiParagraph {
    let step = job.step(authority, ONE).unwrap();
    assert_eq!(step.status, MarkdownBidiPrepareStatus::Ready);
    // The real Gate records borrows and actual operations. Exact whole-source proof
    // finishes in the first quantum with no Reader/Store work.
    assert_eq!(
        (
            step.bytes,
            step.scalars,
            step.metadata_ops,
            step.record_reads,
            step.record_writes,
            step.resource_ops,
            step.source_borrow_max_bytes,
        ),
        (0, 0, 0, 0, 0, 0, 0)
    );
    let actual = job.take_ready(authority).unwrap().unwrap();
    assert!(matches!(actual.resolution, Resolution::UniformZero(_)));
    assert_eq!(actual.storage_profile().source_borrow_max_bytes, 0);
    assert_eq!(actual.storage_profile().cache_bytes, 0);
    assert_eq!(actual.storage_profile().page_loads, 0);
    actual
}

fn assert_unallocated(receipt: &MarkdownBidiResourceObserver) {
    let actual = receipt.snapshot();
    assert!(!actual.file_created && !actual.file_open && !actual.close_succeeded);
    assert_eq!(actual.close_errno, None);
    assert_eq!((actual.cache_bytes, actual.peak_cache_bytes), (0, 0));
    assert!(receipt.is_released()); // Never allocated is distinct from a successful real close; invent no close event.
}

fn assert_ascii_oracle(text: &str, base: BidiBaseDirection, actual: &mut MarkdownBidiParagraph) {
    assert!(text.is_ascii() && text.len() <= 4096);
    // Only this finite oracle builds a complete array; production and the four-MiB
    // fixture do not create complete document copies.
    let units: Vec<u16> = text.encode_utf16().collect();
    let oracle = BidiInfo::new_with_data_source(
        &UnicodeBidiData,
        &units,
        base.level()
            .map(|level| unicode_bidi::Level::new(level).unwrap()),
    );
    assert_eq!(actual.actual_paragraph_count(), oracle.paragraphs.len());
    for (position, expected) in oracle.levels.iter().enumerate() {
        assert_eq!(actual.level_at_utf16(position).unwrap(), expected.number());
    }
    for paragraph in &oracle.paragraphs {
        assert_eq!(
            actual
                .paragraph_range_at_utf16(paragraph.range.start)
                .unwrap(),
            Some(paragraph.range.clone())
        );
        assert_eq!(
            actual
                .paragraph_base_level_at_utf16(paragraph.range.start)
                .unwrap(),
            Some(paragraph.level.number())
        );
        let levels = oracle.reordered_levels_per_char(paragraph, paragraph.range.clone());
        let expected: Vec<_> = BidiInfo::reorder_visual(&levels[paragraph.range.clone()])
            .into_iter()
            .map(|index| paragraph.range.start + index)
            .collect();
        let mut observed = Vec::new();
        actual
            .visit_visual_runs(paragraph.range.clone(), |run| {
                // ASCII units equal scalars; apply actual L1/L2 reversal according to each run's level.
                let range = run.source_utf16_start..run.source_utf16_end;
                if run.level & 1 == 0 {
                    observed.extend(range);
                } else {
                    observed.extend(range.rev());
                }
            })
            .unwrap();
        assert_eq!(observed, expected);
    }
    assert_eq!(actual.paragraph_range_at_utf16(text.len()).unwrap(), None);
}

#[test]
fn exact_full_ascii_is_zero_read_and_matches_complete_uba_oracle() {
    // Cover the complete printable domain and mixed numeric/punctuation contexts in
    // both accepted bases, comparing actual levels, paragraph ranges, and visual runs.
    let all_printable: String = (b' '..=b'~').map(char::from).collect();
    for text in [
        "",
        "1234 + 56.78, ($9) [10] {11} <> -- / :!?",
        "   () [] {} \"' -+ 123 %$ #@   ",
        "a [b (12)] {c} !",
        &all_printable,
    ] {
        for base in [BidiBaseDirection::Auto, BidiBaseDirection::LeftToRight] {
            let fixture = Fixture::new(text);
            let (authority, mut preparation, receipt) =
                job(fixture.source(), base, &MarkdownBidiCancellation::new());
            let mut actual = fast_ready(&mut preparation, &authority);
            assert_ascii_oracle(text, base, &mut actual);
            actual.require_layout_source(&authority).unwrap();
            assert_unallocated(&actual.resource_observer());
            drop(actual);
            drop(preparation);
            assert_unallocated(&receipt);
        }
    }
}

#[test]
fn four_mebibyte_single_paragraph_needs_no_uba_source_scan_or_metadata_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("large-printable.md");
    let mut file = std::fs::File::create(&path).unwrap();
    for _ in 0..1024 {
        // Write a real four-MiB file with fixed 4 KiB blocks, rather than replacing the
        // lazy source workload with a repeated String or complete Vec.
        file.write_all(&[b'a'; 4096]).unwrap();
    }
    drop(file);
    let document = MarkdownDocumentBuffer::open(&path, 8 * 1024 * 1024).unwrap();
    let source = MarkdownLayoutTextSource::capture(&document);
    let payload = source.resource_observer();
    let mapping = document.original_backing_observer().unwrap();
    let reclaim_before = mapping.reclaim_attempts();
    let (authority, mut preparation, receipt) = job(
        source,
        BidiBaseDirection::Auto,
        &MarkdownBidiCancellation::new(),
    );
    drop(document); // Only the original Future/result retains frozen Source; no live document supplies hidden fallback ownership.
    let mut actual = fast_ready(&mut preparation, &authority);
    assert_eq!(mapping.reclaim_attempts(), reclaim_before);
    assert_eq!(actual.source_range(), 0..4 * 1024 * 1024);
    for position in [0, 2 * 1024 * 1024, 4 * 1024 * 1024 - 1] {
        assert_eq!(actual.level_at_utf16(position).unwrap(), 0);
        assert_eq!(
            actual.paragraph_range_at_utf16(position).unwrap(),
            Some(0..4 * 1024 * 1024)
        );
    }
    assert!(!payload.payload_released());
    assert_unallocated(&receipt);
    drop(actual);
    drop(preparation);
    assert!(payload.payload_released());
    assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
}

#[test]
fn frozen_root_proof_survives_edit_but_never_authorizes_new_or_foreign_source() {
    // The old frozen source remains exact after editing, while edited, restored, and
    // separately recaptured sources must retain their own distinct authority.
    let mut fixture = Fixture::new("abcdef (123)");
    let old = fixture.source();
    let old_authority = old.authority();
    fixture
        .document
        .insert_utf16(3, "א", MarkdownTextSelection::caret(4))
        .unwrap();
    let edited = fixture.source();
    assert!(old_authority != edited.authority());
    let (old_authority, mut old_job, _) = job(
        old,
        BidiBaseDirection::Auto,
        &MarkdownBidiCancellation::new(),
    );
    let mut old_actual = fast_ready(&mut old_job, &old_authority);
    assert_ascii_oracle("abcdef (123)", BidiBaseDirection::Auto, &mut old_actual);
    let (edited_authority, mut edited_job, _) = job(
        edited,
        BidiBaseDirection::Auto,
        &MarkdownBidiCancellation::new(),
    );
    let first = edited_job.step(&edited_authority, ONE).unwrap();
    assert_quantum(first);
    assert_eq!(first.status, MarkdownBidiPrepareStatus::Pending);
    assert_eq!(first.bytes, 1); // The edited mixed root loses admission and cannot reuse the old root's zero-level proof.
    assert!(fixture.document.undo().is_some());
    let restored = fixture.source();
    let restored_authority = restored.authority();
    assert!(old_authority != restored_authority);
    let foreign_same_root = fixture.source();
    let (restored_authority, mut restored_job, _) = job(
        restored,
        BidiBaseDirection::Auto,
        &MarkdownBidiCancellation::new(),
    );
    let restored_actual = fast_ready(&mut restored_job, &restored_authority);
    restored_actual
        .require_layout_source(&restored_authority)
        .unwrap();
    assert!(matches!(
        restored_actual.require_layout_source(&old_authority),
        Err(BidiError::SourceMismatch)
    ));
    assert!(matches!(
        restored_actual.require_layout_source(&foreign_same_root.authority()),
        Err(BidiError::SourceMismatch)
    )); // Equal generation, root, and bytes still do not identify the original Source capture.
}

#[test]
fn rtl_paragraphs_and_unproven_unicode_keep_original_cooperative_reader() {
    for (text, base) in [
        ("abc (123)", BidiBaseDirection::RightToLeft),
        ("abc\nsecond", BidiBaseDirection::Auto),
        ("abc\r123", BidiBaseDirection::LeftToRight),
        ("abcא", BidiBaseDirection::Auto),
    ] {
        let fixture = Fixture::new(text);
        let (authority, mut preparation, _) =
            job(fixture.source(), base, &MarkdownBidiCancellation::new());
        let first = preparation.step(&authority, ONE).unwrap();
        assert_quantum(first);
        if text == "abc\nsecond" || text == "abc\r123" {
            assert_eq!(first.status, MarkdownBidiPrepareStatus::Ready);
            let mut actual = preparation.take_ready(&authority).unwrap().unwrap();
            assert_ascii_oracle(text, base, &mut actual);
            continue; // The complete oracle still checks actual B paragraphs; proven level zero only avoids the full Text table.
        }
        assert_eq!(first.status, MarkdownBidiPrepareStatus::Pending);
        assert!(preparation.take_ready(&authority).unwrap().is_none());
        if base == BidiBaseDirection::RightToLeft {
            // RTL immediately declines Uniform. The original Store creates its FD before
            // text reads, so the first quantum has only resource work. Keep that actual
            // order, then complete UBA and compare independent full-paragraph semantics.
            assert_eq!((first.bytes, first.source_borrow_max_bytes), (0, 0));
            assert_eq!((first.metadata_ops, first.resource_ops), (1, 1));
            let mut completed = false;
            for _ in 0..100_000 {
                let step = preparation.step(&authority, ONE).unwrap();
                assert_quantum(step);
                if step.status == MarkdownBidiPrepareStatus::Ready {
                    let mut actual = preparation.take_ready(&authority).unwrap().unwrap();
                    assert!(matches!(actual.resolution, Resolution::Paged(_)));
                    assert_ascii_oracle(text, base, &mut actual);
                    completed = true;
                    break;
                }
            }
            assert!(completed, "有限RTL输入未在原量子推进中完成");
        } else {
            assert_eq!((first.bytes, first.source_borrow_max_bytes), (1, 1));
        }
    }
}

#[test]
fn partial_ranges_read_only_original_one_byte_quanta_even_inside_mixed_leaf() {
    // Partial scopes must use the actual Reader, even inside an otherwise proven or
    // mixed leaf. Every poll retains the original one-byte Gate quantum.
    for (text, source, bytes) in [("abcd", 1..3, 1..3), ("aaa中bbb", 4..7, 6..9)] {
        let fixture = Fixture::new(text);
        let snapshot = fixture.document.text_snapshot();
        let gate = Gate::new(&snapshot, MarkdownBidiCancellation::new());
        let mut preparation = Box::pin(certify_prepared(
            &snapshot,
            &source,
            bytes.clone(),
            BidiBaseDirection::Auto,
            &gate,
        ));
        let mut context = Context::from_waker(Waker::noop());
        gate.replenish(ONE);
        assert!(preparation.as_mut().poll(&mut context).is_pending());
        let first = gate.progress();
        assert_quantum(first);
        assert_eq!((first.bytes, first.scalars), (1, 1));
        let mut total_bytes = first.bytes;
        let mut done = false;
        for _ in 0..16 {
            gate.replenish(ONE);
            let answer = preparation.as_mut().poll(&mut context);
            let step = gate.progress();
            assert_quantum(step);
            total_bytes += step.bytes;
            if let Poll::Ready(answer) = answer {
                assert!(answer.unwrap().is_some());
                done = true;
                break;
            }
        }
        assert!(done); // Finish the actual partial range; its first Pending is not completion.
        assert_eq!(total_bytes, bytes.len());
        assert_eq!(gate.source_borrow_max(), 1);
    }
}

#[test]
fn cancellation_still_revokes_before_and_after_zero_read_certificate() {
    for phase in 0..3 {
        let fixture = Fixture::new("abc (123)");
        let source = fixture.source();
        let authority = source.authority();
        let payload = source.resource_observer();
        let mapping = fixture.document.original_backing_observer().unwrap();
        let cancel = MarkdownBidiCancellation::new();
        if phase == 0 {
            cancel.cancel(); // Preconstruction cancellation cannot use exact aggregates to mint a new certificate.
        }
        let (result, receipt) = MarkdownBidiPrepareJob::new_layout_source_observed(
            source,
            &authority,
            BidiBaseDirection::Auto,
            BidiMemoryBudget::new(65536).unwrap(),
            &cancel,
        );
        if phase == 0 {
            assert!(matches!(result, Err(BidiError::Cancelled)));
        } else {
            let mut preparation = result.unwrap();
            if phase == 1 {
                cancel.cancel(); // Cancel after capture but before polling without borrowing text to finish the Future.
                assert!(matches!(
                    preparation.step(&authority, ONE),
                    Err(BidiError::Cancelled)
                ));
                assert_eq!(preparation.last_step().bytes, 0);
            } else {
                let mut actual = fast_ready(&mut preparation, &authority);
                cancel.cancel();
                assert!(matches!(
                    actual.level_at_utf16(0),
                    Err(BidiError::Cancelled)
                ));
            }
            drop(preparation);
        }
        drop(fixture);
        assert_unallocated(&receipt);
        assert!(payload.payload_released());
        assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
    }
}

#[test]
fn actual_backing_eio_precedes_cancel_foreign_authority_and_ready_cache() {
    for ready in [false, true] {
        let fixture = Fixture::new("abc (123)");
        let source = fixture.source();
        let payload = source.resource_observer();
        let mapping = fixture.document.original_backing_observer().unwrap();
        let cancel = MarkdownBidiCancellation::new();
        let (authority, mut preparation, receipt) =
            job(source.clone(), BidiBaseDirection::Auto, &cancel);
        if ready {
            let first = preparation.step(&authority, ONE).unwrap();
            assert_eq!(first.status, MarkdownBidiPrepareStatus::Ready);
            assert_eq!(first.bytes, 0); // Retain a real cached Ready, which the later actual source failure must revoke.
        }
        source.visit_byte_range(0..1, |_| Ok(())).unwrap();
        fixture.document.fail_next_original_reclaim_for_test();
        assert_eq!(
            fixture.document.reclaim_original_read_windows_for_test(),
            Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                libc::EIO
            ))
        ); // Actual original-page reclaim failure establishes EIO; setting an injection flag alone does not.
        cancel.cancel();
        let foreign = fixture.source().authority();
        assert!(matches!(
            preparation.step(&foreign, ONE),
            Err(BidiError::Source(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
            ))
        ));
        assert!(matches!(
            preparation.take_ready(&foreign),
            Err(BidiError::Source(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
            ))
        ));
        assert_unallocated(&receipt);
        drop(preparation);
        drop(source);
        drop(fixture);
        assert!(payload.payload_released());
        assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0);
        assert_eq!(mapping.first_errno(), Some(libc::EIO));
    }
}
