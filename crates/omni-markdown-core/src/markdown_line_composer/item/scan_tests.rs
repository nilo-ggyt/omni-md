//! Exercise whole-item fast paths through actual Uniform proofs, frozen aggregates,
//! and original dependencies. Unproven/control-bearing text retains original handling.

use super::*;

struct Fixture {
    _directory: tempfile::TempDir,
    document: crate::MarkdownDocumentBuffer,
    source: MarkdownLayoutTextSource,
    worker: MarkdownLineItemCoordinator,
    cancellation: MarkdownLineComposeCancellation,
}

impl Fixture {
    fn at_scan(text: &str) -> Self {
        Self::at_stage(text, |stage| matches!(stage, Stage::ItemScan))
    }

    fn at_start(text: &str) -> Self {
        Self::at_stage(text, |stage| matches!(stage, Stage::ItemStart))
    }

    fn at_stage(text: &str, requested: fn(&Stage) -> bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("真实分项扫描.md");
        std::fs::write(&path, text).unwrap();
        let document =
            crate::MarkdownDocumentBuffer::open(&path, text.len() as u64 + 65536).unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let cancellation = MarkdownLineComposeCancellation::new();
        let mut worker = MarkdownLineItemCoordinator::new(
            source.clone(),
            &source.authority(),
            cancellation.clone(),
        )
        .unwrap();
        // Observe the actual requested preparation boundary without changing it.
        // Original UBA/Script_Extensions/grapheme/line-break work runs normally first.
        for _ in 0..100_000 {
            if worker
                .payload
                .as_ref()
                .is_some_and(|payload| requested(&payload.stage))
            {
                return Self {
                    _directory: directory,
                    document,
                    source,
                    worker,
                    cancellation,
                };
            }
            assert!(matches!(
                worker
                    .step(MarkdownLineComposeBudget::default())
                    .unwrap()
                    .answer,
                MarkdownLineItemAnswer::Yielded
            ));
        }
        panic!("有限真实夹具没有到达请求的原阶段");
    }
}

#[test]
fn unicode_uniform_item_start_and_scan_reuse_real_control_proof_without_new_source_reads() {
    // Original Script_Extensions makes the first fixture's Hani run exactly 中文;
    // its emoji/mark follows in Common. The second fixture is Common throughout,
    // with nine UTF-16 units per repeat. Expected ranges are independent of responses.
    for (text, first_script_range) in [
        ("中文😀\u{0301} ".repeat(128), 0..2),
        ("👩🏽‍💻🙂".repeat(64), 0..9 * 64),
    ] {
        let mut fixture = Fixture::at_start(&text);
        let source_receipt = fixture.source.resource_observer();
        let mapping = fixture.source.original_backing_observer().unwrap();
        fixture
            .document
            .reclaim_original_read_windows_for_test()
            .unwrap();
        // Reclaim actual Source pages before the operation, then observe whether
        // its already certified metadata can answer without faulting those pages back.
        let before = (mapping.managed_resident_bytes(), mapping.reclaim_attempts());
        let start = fixture.worker.step(one_metadata()).unwrap();
        assert!(matches!(start.answer, MarkdownLineItemAnswer::Yielded));
        assert_eq!(
            (
                start.bytes,
                start.scalars,
                start.records,
                start.metadata_units,
                start.shapes
            ),
            (0, 0, 0, 1, 0)
        );
        assert_eq!(
            (mapping.managed_resident_bytes(), mapping.reclaim_attempts()),
            before
        );
        let scan = fixture.worker.step(one_metadata()).unwrap();
        let MarkdownLineItemAnswer::NeedStyle(request) = scan.answer else {
            panic!("原Unicode Uniform无控制证书应在一个metadata量子完成真实项交集");
        };
        assert_eq!(request.source_range(), first_script_range);
        assert_eq!(request.paragraph_range(), 0..fixture.source.utf16_len());
        assert_eq!(request.level(), 0);
        assert_eq!(
            (
                scan.bytes,
                scan.scalars,
                scan.records,
                scan.metadata_units,
                scan.shapes
            ),
            (0, 0, 0, 1, 0)
        );
        assert_eq!(
            (mapping.managed_resident_bytes(), mapping.reclaim_attempts()),
            before
        );
        drop((request, fixture));
        assert!(source_receipt.payload_released());
        assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
    }
}

#[test]
fn unicode_uniform_proof_stops_at_real_script_intersection() {
    let mut fixture = Fixture::at_scan("中文abcdef");
    let result = fixture.worker.step(one_metadata()).unwrap();
    let MarkdownLineItemAnswer::NeedStyle(request) = result.answer else {
        panic!("真实Hani项应完成");
    };
    assert_eq!(request.source_range(), 0..2);
    assert_eq!(request.paragraph_range(), 0..8);
    assert_eq!(request.script(), *b"Hani"); // A whole Uniform proof cannot absorb the later Latin run.
}

#[test]
fn original_six_controls_and_crlf_keep_their_exact_control_ranges_and_kinds() {
    // These are actual public control requests from real original Sources. Preserve
    // both CRLF units while keeping its original CarriageReturn control kind.
    for (prefix, kind, end) in [
        ("\t", MarkdownLineInlineControlKind::Tab, 1),
        ("\r", MarkdownLineInlineControlKind::CarriageReturn, 1),
        ("\n", MarkdownLineInlineControlKind::LineFeed, 1),
        ("\u{0085}", MarkdownLineInlineControlKind::NextLine, 1),
        ("\u{2028}", MarkdownLineInlineControlKind::LineSeparator, 1),
        (
            "\u{2029}",
            MarkdownLineInlineControlKind::ParagraphSeparator,
            1,
        ),
        ("\r\n", MarkdownLineInlineControlKind::CarriageReturn, 2),
    ] {
        let mut fixture = Fixture::at_start(&format!("{prefix}中文"));
        let result = fixture.worker.step(one_metadata()).unwrap();
        let MarkdownLineItemAnswer::NeedsControl(control) = result.answer else {
            panic!("原控制不得被完整Uniform的level0吞掉：{prefix:?}");
        };
        assert_eq!(control.source_utf16, 0..end);
        assert_eq!(control.kind, kind);
        assert!(control.source == fixture.source.authority());
    }
}

#[test]
fn controls_inside_uniform_range_still_split_before_original_control_instead_of_skipping_it() {
    for scalar in ['\t', '\u{2028}'] {
        let mut fixture = Fixture::at_scan(&format!("中文{scalar}后继"));
        let first = fixture.worker.step(one_metadata()).unwrap();
        assert!(matches!(first.answer, MarkdownLineItemAnswer::Yielded));
        let second = fixture.worker.step(one_metadata()).unwrap();
        let MarkdownLineItemAnswer::NeedStyle(request) = second.answer else {
            panic!("原真实控制前的Hani项应停止于控制起点");
        };
        assert_eq!(request.source_range(), 0..2);
    } // A control-bearing Uniform range has no control-free certificate; the
    // original per-unit path must stop the public style range before that control.
}

#[test]
fn unicode_uniform_source_error_and_cancel_still_revoke_the_real_coordinator_owner() {
    for at_start in [true, false] {
        let mut fixture = if at_start {
            Fixture::at_start(&"中文😀 ".repeat(32))
        } else {
            Fixture::at_scan(&"中文😀 ".repeat(32))
        };
        fixture.source.visit_byte_range(0..1, |_| Ok(())).unwrap();
        // Existing injection makes actual backing reclaim return EIO; it is not a
        // naturally induced kernel failure. Later cancellation must preserve it.
        fixture.document.fail_next_original_reclaim_for_test();
        assert_eq!(
            fixture.document.reclaim_original_read_windows_for_test(),
            Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                libc::EIO
            ))
        );
        fixture.cancellation.cancel();
        assert!(matches!(
            fixture.worker.step(one_metadata()),
            Err(MarkdownLineComposeError::Source(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
            ))
        ));
    }
}

fn one_metadata() -> MarkdownLineComposeBudget {
    MarkdownLineComposeBudget {
        bytes: 4096,
        scalars: 256,
        records: 256,
        metadata_units: 1,
    }
}

#[test]
fn exact_whole_source_printable_aggregate_skips_giant_item_in_one_metadata_turn() {
    for text in [
        "abcd!? 0123 ".repeat(1024),
        "0123!? ".repeat(1024),
        "%[]()=+".repeat(512),
    ] {
        let mut fixture = Fixture::at_scan(&text);
        let source_receipt = fixture.source.resource_observer();
        let mapping = fixture.source.original_backing_observer().unwrap();
        fixture
            .document
            .reclaim_original_read_windows_for_test()
            .unwrap();
        let before = (mapping.managed_resident_bytes(), mapping.reclaim_attempts());
        let step = fixture.worker.step(one_metadata()).unwrap();
        let MarkdownLineItemAnswer::NeedStyle(request) = step.answer else {
            panic!("原精确全源证明不应继续逐字符扫描");
        };
        assert_eq!(request.source_range(), 0..fixture.source.utf16_len());
        assert_eq!(request.paragraph_range(), request.source_range());
        assert_eq!(request.level(), 0);
        assert_eq!(
            (
                step.bytes,
                step.scalars,
                step.records,
                step.metadata_units,
                step.shapes
            ),
            (0, 0, 0, 1, 0)
        );
        assert_eq!(
            (mapping.managed_resident_bytes(), mapping.reclaim_attempts()),
            before
        );
        drop((request, fixture));
        assert!(source_receipt.payload_released());
        assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
    } // Actual Script results govern Latin, Common, and paired punctuation alike;
    // a successful aggregate must not relabel Common as Latin.
}

#[test]
fn source_error_still_precedes_cancel_before_printable_aggregate_can_publish_style() {
    let mut fixture = Fixture::at_scan(&"abcd ".repeat(256));
    fixture
        .document
        .text_snapshot()
        .visit_byte_range(0..1, |_| Ok(()))
        .unwrap();
    fixture.document.fail_next_original_reclaim_for_test();
    assert_eq!(
        fixture.document.reclaim_original_read_windows_for_test(),
        Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
            libc::EIO
        ))
    );
    fixture.cancellation.cancel();
    assert!(matches!(
        fixture.worker.step(one_metadata()),
        Err(MarkdownLineComposeError::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
}
