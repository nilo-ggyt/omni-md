//! Unicode根摘要只授予原Uniform语义；有限完整UBA oracle与实际源/资源收据分别核验。

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

const ONE: MarkdownBidiPrepareBudget = MarkdownBidiPrepareBudget {
    bytes: 1,
    scalars: 1,
    metadata_ops: 1,
};

fn fixture(text: &str) -> (tempfile::TempDir, MarkdownDocumentBuffer) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("uniform-properties.md");
    std::fs::write(&path, text).unwrap();
    let document = MarkdownDocumentBuffer::open(&path, 8 * 1024 * 1024).unwrap();
    (directory, document)
}

fn preparation(
    source: MarkdownLayoutTextSource,
    base: BidiBaseDirection,
    cancellation: &MarkdownBidiCancellation,
) -> (
    MarkdownLayoutTextAuthority,
    MarkdownBidiPrepareJob,
    MarkdownBidiResourceObserver,
) {
    let authority = source.authority();
    let (job, receipt) = MarkdownBidiPrepareJob::new_layout_source_observed(
        source,
        &authority,
        base,
        BidiMemoryBudget::new(65536).unwrap(),
        cancellation,
    );
    (authority, job.unwrap(), receipt)
}

fn fast(
    job: &mut MarkdownBidiPrepareJob,
    authority: &MarkdownLayoutTextAuthority,
) -> MarkdownBidiParagraph {
    let step = job.step(authority, ONE).unwrap();
    assert_eq!(step.status, MarkdownBidiPrepareStatus::Ready);
    assert_eq!(
        (
            step.bytes,
            step.scalars,
            step.metadata_ops,
            step.source_borrow_max_bytes
        ),
        (0, 0, 0, 0)
    );
    assert_eq!(
        (step.record_reads, step.record_writes, step.resource_ops),
        (0, 0, 0)
    );
    let actual = job.take_ready(authority).unwrap().unwrap();
    assert!(matches!(actual.resolution, Resolution::UniformZero(_)));
    assert_eq!(actual.storage_profile().source_borrow_max_bytes, 0);
    actual
}

fn unallocated(receipt: &MarkdownBidiResourceObserver) {
    let facts = receipt.snapshot();
    assert!(!facts.file_created && !facts.file_open && !facts.close_succeeded);
    assert_eq!((facts.cache_bytes, facts.peak_cache_bytes), (0, 0));
    assert!(receipt.is_released()); // 没创建FD不同于创建后close，不能用合成关闭事件作收据。
}

#[test]
fn complete_eligible_unicode_matches_original_uba_for_every_class_and_mixed_context() {
    for text in [
        "中",
        "😀",
        "\u{301}",
        "\u{ad}",
        "\t",
        "\u{2028}",
        "\u{1f}",
        "中文 (123 + 4.56, $7) [😀] e\u{301}\u{ad}",
        "\u{301}\u{ad}123\u{2028}中文\t()[]\u{200e}",
        "\u{05c8}\u{1acf}\u{1b3a}\u{1ccfa}\u{2065}\u{10ffff}",
    ] {
        for base in [BidiBaseDirection::Auto, BidiBaseDirection::LeftToRight] {
            let (_directory, document) = fixture(text);
            let source = MarkdownLayoutTextSource::capture(&document);
            let (authority, mut job, receipt) =
                preparation(source.clone(), base, &MarkdownBidiCancellation::new());
            let mut actual = fast(&mut job, &authority);
            control_tests::assert_oracle(text, base, &mut actual);
            let control_free = !text.chars().any(|scalar| {
                matches!(
                    scalar,
                    '\t' | '\r' | '\n' | '\u{85}' | '\u{2028}' | '\u{2029}'
                )
            });
            assert_eq!(
                actual
                    .uniform_control_free_span(&authority, 0..source.utf16_len())
                    .unwrap()
                    .is_some(),
                control_free
            );
            let synchronous = MarkdownBidiParagraph::analyze_layout_source(
                source.clone(),
                &authority,
                0..source.utf16_len(),
                base,
                BidiMemoryBudget::new(65536).unwrap(),
            )
            .unwrap();
            assert_eq!(
                synchronous
                    .uniform_control_free_span(&authority, 0..source.utf16_len())
                    .unwrap()
                    .is_some(),
                control_free
            );
            unallocated(&receipt);
        }
    }
}

#[test]
fn actual_b_ranges_and_ineligible_classes_preserve_original_uba_semantics() {
    for (text, zero_level_proven) in [
        ("中\n", true),
        ("\n中", true),
        ("中\r文", true),
        ("中\u{85}", true),
        ("中\u{2029}", true),
        ("中א", false),
        ("中ش", false),
        ("中١", false),
        ("中\u{202b}a\u{202c}", false),
        ("中\u{2067}א\u{2069}", false),
    ] {
        for base in [
            BidiBaseDirection::Auto,
            BidiBaseDirection::LeftToRight,
            BidiBaseDirection::RightToLeft,
        ] {
            let (_directory, document) = fixture(text);
            let source = MarkdownLayoutTextSource::capture(&document);
            let (authority, mut job, _) =
                preparation(source.clone(), base, &MarkdownBidiCancellation::new());
            let first = job.step(&authority, ONE).unwrap();
            if zero_level_proven && base != BidiBaseDirection::RightToLeft {
                assert_eq!(first.status, MarkdownBidiPrepareStatus::Ready);
                assert_eq!((first.bytes, first.scalars, first.metadata_ops), (0, 0, 0));
                let mut actual = job.take_ready(&authority).unwrap().unwrap();
                control_tests::assert_oracle(text, base, &mut actual);
                continue; // 新表示只省层级表；原每个B范围及L1/L2仍由同一个完整oracle核验。
            }
            assert_eq!(first.status, MarkdownBidiPrepareStatus::Pending);
            assert!(job.take_ready(&authority).unwrap().is_none());
            let mut reads = first.bytes;
            let mut ready = None;
            for _ in 0..100_000 {
                let step = job.step(&authority, ONE).unwrap();
                assert!(step.bytes <= 1 && step.scalars <= 1 && step.metadata_ops <= 1);
                assert!(step.source_borrow_max_bytes <= 1);
                assert_eq!(
                    step.metadata_ops,
                    step.record_reads + step.record_writes + step.resource_ops
                );
                reads += step.bytes;
                if step.status == MarkdownBidiPrepareStatus::Ready {
                    ready = job.take_ready(&authority).unwrap();
                    break;
                }
            }
            let mut actual = ready.expect("未证明输入必须在原预算算法中真正完成");
            assert!(reads >= text.len());
            control_tests::assert_oracle(text, base, &mut actual);
        }
    }
}

#[test]
fn partial_unicode_ranges_never_reuse_full_root_summary_or_bypass_one_byte_budget() {
    for (text, source, bytes) in [
        ("甲中文乙", 1..3, 3..9),
        ("א中文ש", 1..3, 2..8),
        ("中文\n", 0..2, 0..6),
    ] {
        let (_directory, document) = fixture(text);
        let snapshot = document.text_snapshot();
        let gate = Gate::new(&snapshot, MarkdownBidiCancellation::new());
        let mut future = Box::pin(certify_prepared(
            &snapshot,
            &source,
            bytes.clone(),
            BidiBaseDirection::Auto,
            &gate,
        ));
        let mut context = Context::from_waker(Waker::noop());
        let mut reads = 0;
        let mut finished = false;
        for turn in 0..32 {
            gate.replenish(ONE);
            let result = future.as_mut().poll(&mut context);
            let work = gate.progress();
            assert!(work.bytes <= 1 && work.scalars <= 1 && work.metadata_ops <= 1);
            assert!(work.source_borrow_max_bytes <= 1);
            reads += work.bytes;
            if turn == 0 {
                assert!(result.is_pending());
                assert_eq!(reads, 1); // 即使整root可证明或范围恰好排除RTL/B，也必须真实扫描局部源。
            }
            if let Poll::Ready(result) = result {
                assert!(result.unwrap().is_some());
                finished = true;
                break;
            }
        }
        assert!(finished);
        assert_eq!(reads, bytes.len());
    }
}

#[test]
fn canonical_certificate_matches_the_unchanged_reader_on_complete_unicode_ranges() {
    for text in [
        "",
        "中文😀e\u{301}",
        "中\u{ad}\u{05c8}\u{1b3a}\u{1ccfa}",
        "中\t文\u{2028}",
        "\u{1f}中",
        "中א",
        "中ش",
        "中١",
        "中\u{202b}a\u{202c}",
    ] {
        let (_directory, document) = fixture(text);
        let snapshot = document.text_snapshot();
        let range = 0..snapshot.utf16_len();
        let gate = Gate::new(&snapshot, MarkdownBidiCancellation::new());
        let mut original = Box::pin(certify_scanned(
            &snapshot,
            &range,
            0..snapshot.byte_len(),
            &gate,
        ));
        let mut context = Context::from_waker(Waker::noop());
        let mut bytes = 0;
        let mut observed = None;
        for _ in 0..256 {
            gate.replenish(ONE);
            let result = original.as_mut().poll(&mut context);
            let step = gate.progress();
            assert!(step.bytes <= 1 && step.scalars <= 1 && step.metadata_ops <= 1);
            bytes += step.bytes;
            if let Poll::Ready(certificate) = result {
                observed = Some(certificate.unwrap().map(|certificate| {
                    certificate
                        .seal(MarkdownBidiCancellation::new())
                        .control_free()
                }));
                break;
            }
        }
        let original_fact = observed.expect("原Reader必须真实完成，不能把Pending视为None");
        if original_fact.is_some() {
            assert_eq!(bytes, text.len()); // 独立旧路径确实读完整源，与摘要快路的零读取区别计量。
        } else {
            assert!(bytes > 0); // 遇到原拒绝类可以提前停，不能要求它为测试继续读后缀。
        }
        let gate = Gate::new(&snapshot, MarkdownBidiCancellation::new());
        let mut candidate = Box::pin(certify_prepared(
            &snapshot,
            &range,
            0..snapshot.byte_len(),
            BidiBaseDirection::Auto,
            &gate,
        ));
        let mut candidate_fact = None;
        for _ in 0..256 {
            gate.replenish(ONE);
            if let Poll::Ready(certificate) = candidate.as_mut().poll(&mut context) {
                candidate_fact = Some(certificate.unwrap().map(|certificate| {
                    certificate
                        .seal(MarkdownBidiCancellation::new())
                        .control_free()
                }));
                break;
            }
        }
        assert_eq!(candidate_fact.expect("新路径必须真实完成"), original_fact);
    }
}

#[test]
fn actual_four_mebibyte_unicode_paragraph_uses_no_second_source_scan_and_releases_mapping() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("large-chinese.md");
    let mut file = std::fs::File::create(&path).unwrap();
    let chunk = "中文😀 ".repeat(372);
    for _ in 0..1024 {
        file.write_all(chunk.as_bytes()).unwrap(); // 有限≤4KiB写块，真实大文件不先组装全文String。
    }
    file.write_all(&[b'a'; 4096]).unwrap();
    drop(file);
    let document = MarkdownDocumentBuffer::open(&path, 8 * 1024 * 1024).unwrap();
    let source = MarkdownLayoutTextSource::capture(&document);
    assert_eq!(source.byte_len(), 4 * 1024 * 1024);
    let length = source.utf16_len();
    let payload = source.resource_observer();
    let mapping = document.original_backing_observer().unwrap();
    document.reclaim_original_read_windows_for_test().unwrap();
    let before = (mapping.managed_resident_bytes(), mapping.reclaim_attempts());
    let (authority, mut job, receipt) = preparation(
        source,
        BidiBaseDirection::Auto,
        &MarkdownBidiCancellation::new(),
    );
    drop(document);
    let actual = fast(&mut job, &authority);
    assert_eq!(
        (mapping.managed_resident_bytes(), mapping.reclaim_attempts()),
        before
    );
    let span = actual
        .uniform_control_free_span(&authority, 0..length)
        .unwrap()
        .unwrap();
    assert_eq!(span.source_range(), 0..length);
    assert_eq!(span.paragraph_range(), 0..length);
    unallocated(&receipt);
    drop((actual, job));
    assert!(payload.payload_released());
    assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
}

#[test]
fn frozen_unicode_root_and_undo_never_authorize_changed_or_foreign_sources() {
    let (_directory, mut document) = fixture("中文😀 (123)");
    let old = MarkdownLayoutTextSource::capture(&document);
    let (authority, mut job, _) = preparation(
        old.clone(),
        BidiBaseDirection::Auto,
        &MarkdownBidiCancellation::new(),
    );
    let old_actual = fast(&mut job, &authority);
    document
        .insert_utf16(0, "א", MarkdownTextSelection::caret(1))
        .unwrap();
    let edited = MarkdownLayoutTextSource::capture(&document);
    assert!(matches!(
        old_actual.require_layout_source(&edited.authority()),
        Err(BidiError::SourceMismatch)
    ));
    let (changed_authority, mut changed_job, _) = preparation(
        edited,
        BidiBaseDirection::Auto,
        &MarkdownBidiCancellation::new(),
    );
    assert_eq!(
        changed_job.step(&changed_authority, ONE).unwrap().status,
        MarkdownBidiPrepareStatus::Pending
    );
    document.undo_text_edit().unwrap();
    let restored = MarkdownLayoutTextSource::capture(&document);
    let (restored_authority, mut restored_job, _) = preparation(
        restored,
        BidiBaseDirection::Auto,
        &MarkdownBidiCancellation::new(),
    );
    let restored_actual = fast(&mut restored_job, &restored_authority);
    assert!(matches!(
        restored_actual.require_layout_source(&authority),
        Err(BidiError::SourceMismatch)
    ));
    let foreign = MarkdownLayoutTextSource::capture(&document);
    assert!(matches!(
        restored_actual.require_layout_source(&foreign.authority()),
        Err(BidiError::SourceMismatch)
    ));
    old_actual.require_layout_source(&authority).unwrap();
}

#[test]
fn cached_unicode_summary_respects_cancellation_and_original_eio_priority() {
    for ready in [false, true] {
        let (_directory, document) = fixture("中文😀");
        let source = MarkdownLayoutTextSource::capture(&document);
        let mapping = document.original_backing_observer().unwrap();
        let payload = source.resource_observer();
        let cancel = MarkdownBidiCancellation::new();
        let (authority, mut job, receipt) =
            preparation(source.clone(), BidiBaseDirection::Auto, &cancel);
        let actual = ready.then(|| fast(&mut job, &authority));
        source.visit_byte_range(0..1, |_| Ok(())).unwrap();
        document.fail_next_original_reclaim_for_test();
        assert_eq!(
            document.reclaim_original_read_windows_for_test(),
            Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                libc::EIO
            ))
        );
        cancel.cancel();
        let foreign = MarkdownLayoutTextSource::capture(&document).authority();
        assert!(matches!(
            job.step(&foreign, ONE),
            Err(BidiError::Source(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
            ))
        ));
        if let Some(actual) = &actual {
            assert!(matches!(
                actual.uniform_control_free_span(&foreign, 0..source.utf16_len()),
                Err(BidiError::Source(
                    MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
                ))
            ));
        }
        unallocated(&receipt);
        drop((actual, job, source, document));
        assert!(payload.payload_released());
        assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0);
        assert_eq!(mapping.first_errno(), Some(libc::EIO));
    }
    let (_directory, document) = fixture("中文😀");
    let source = MarkdownLayoutTextSource::capture(&document);
    let cancel = MarkdownBidiCancellation::new();
    let (authority, mut job, receipt) = preparation(source, BidiBaseDirection::Auto, &cancel);
    cancel.cancel();
    assert!(matches!(
        job.step(&authority, ONE),
        Err(BidiError::Cancelled)
    ));
    assert_eq!(job.last_step().bytes, 0);
    unallocated(&receipt);
}
