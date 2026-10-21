//! Control facts retain the original complete Uniform semantics. Independent UBA oracles and
//! actual source receipts keep text-read accounting separate from source authority.

use super::*;
use crate::{MarkdownDocumentBuffer, MarkdownLayoutTextSource, MarkdownTextSelection};
use unicode_bidi::utf16::BidiInfo;

const ONE: MarkdownBidiPrepareBudget = MarkdownBidiPrepareBudget {
    bytes: 1,
    scalars: 1,
    metadata_ops: 1,
};

fn fixture(text: &str) -> (tempfile::TempDir, MarkdownDocumentBuffer) {
    // Use an isolated real file so source ownership, immutable reads, and final unmap
    // are exercised without accessing production documents.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("uniform-control.md");
    std::fs::write(&path, text).unwrap();
    let document = MarkdownDocumentBuffer::open(&path, 8 * 1024 * 1024).unwrap();
    (directory, document)
}

fn prepared(
    source: MarkdownLayoutTextSource,
    base: BidiBaseDirection,
    cancel: &MarkdownBidiCancellation,
) -> (MarkdownBidiParagraph, usize) {
    let authority = source.authority();
    let mut job = MarkdownBidiPrepareJob::new_layout_source(
        source,
        &authority,
        base,
        BidiMemoryBudget::new(65536).unwrap(),
        cancel,
    )
    .unwrap();
    // Each independent work dimension has a one-unit quantum. Only actual Ready may
    // publish the completed paragraph; an unfinished Future supplies no partial proof.
    let mut bytes = 0;
    for _ in 0..100_000 {
        let step = job.step(&authority, ONE).unwrap();
        assert!(step.bytes <= 1 && step.scalars <= 1 && step.metadata_ops <= 1);
        assert_eq!(
            step.metadata_ops,
            step.record_reads + step.record_writes + step.resource_ops
        );
        assert!(step.source_borrow_max_bytes <= 1);
        bytes += step.bytes;
        if step.status == MarkdownBidiPrepareStatus::Ready {
            return (job.take_ready(&authority).unwrap().unwrap(), bytes);
        }
        assert!(job.take_ready(&authority).unwrap().is_none()); // An unfinished Future publishes no partial control facts.
    }
    panic!("有限控制夹具未在原量子预算内完成");
}

pub(super) fn assert_oracle(
    text: &str,
    base: BidiBaseDirection,
    actual: &mut MarkdownBidiParagraph,
) {
    assert!(text.chars().count() <= 4096);
    // This finite independent oracle owns a full UTF-16 array; production preparation
    // and long-document fixtures do not adopt that representation.
    let units: Vec<u16> = text.encode_utf16().collect();
    let oracle = BidiInfo::new_with_data_source(
        &UnicodeBidiData,
        &units,
        base.level()
            .map(|level| unicode_bidi::Level::new(level).unwrap()),
    );
    assert_eq!(actual.actual_paragraph_count(), oracle.paragraphs.len());
    for (position, level) in oracle.levels.iter().enumerate() {
        assert_eq!(actual.level_at_utf16(position).unwrap(), level.number());
    }
    let mut offsets = Vec::new();
    let mut offset = 0;
    for scalar in text.chars() {
        offsets.push(offset);
        offset += scalar.len_utf16();
    }
    offsets.push(offset);
    // Compare actual visual scalar order, including L1/L2 effects, rather than treating
    // matching per-unit levels as sufficient evidence for run ordering.
    for paragraph in &oracle.paragraphs {
        let first = offsets.binary_search(&paragraph.range.start).unwrap();
        let last = offsets.binary_search(&paragraph.range.end).unwrap();
        let levels = oracle.reordered_levels_per_char(paragraph, paragraph.range.clone());
        let expected: Vec<_> = BidiInfo::reorder_visual(&levels[first..last])
            .into_iter()
            .map(|index| offsets[first + index])
            .collect();
        let mut observed = Vec::new();
        actual
            .visit_visual_runs(paragraph.range.clone(), |run| {
                let from = offsets.binary_search(&run.source_utf16_start).unwrap();
                let to = offsets.binary_search(&run.source_utf16_end).unwrap();
                if run.level & 1 == 0 {
                    observed.extend_from_slice(&offsets[from..to]);
                } else {
                    observed.extend(offsets[from..to].iter().rev().copied());
                }
            })
            .unwrap();
        assert_eq!(observed, expected);
    }
}

#[test]
fn unicode_uniform_root_summary_seals_control_fact_without_any_source_read() {
    // Exact properties maintained by original open/edit traversal can certify the
    // complete frozen root without a second scan or a UBA metadata file.
    for text in [
        "中文长段落",
        "中😀a\u{0301}\u{00ad}",
        "中\u{200e}文",
        "👩🏽‍💻🙂",
    ] {
        let (_directory, document) = fixture(text);
        let source = MarkdownLayoutTextSource::capture(&document);
        let authority = source.authority();
        let mapping = source.original_backing_observer().unwrap();
        let payload = source.resource_observer();
        let (mut actual, bytes) = prepared(
            source.clone(),
            BidiBaseDirection::Auto,
            &MarkdownBidiCancellation::new(),
        );
        assert!(matches!(actual.resolution, Resolution::UniformZero(_)));
        assert_eq!(bytes, 0); // Existing exact open/edit properties require no repeated Unicode-text scan.
        assert_eq!(actual.storage_profile().source_borrow_max_bytes, 0);
        assert_oracle(text, BidiBaseDirection::Auto, &mut actual);
        document.reclaim_original_read_windows_for_test().unwrap();
        let before = (mapping.managed_resident_bytes(), mapping.reclaim_attempts());
        let span = actual
            .uniform_control_free_span(&authority, 0..source.utf16_len())
            .unwrap()
            .unwrap();
        assert_eq!(span.source_range(), 0..source.utf16_len());
        assert_eq!(span.level(), 0);
        assert_eq!(
            (mapping.managed_resident_bytes(), mapping.reclaim_attempts()),
            before
        );
        // The completed value remains the last source owner after the document and
        // external clone drop; only its destruction may complete the original unmap.
        let receipt = actual.resource_observer();
        assert!(!receipt.snapshot().file_created && receipt.snapshot().cache_bytes == 0);
        drop(document);
        drop(source);
        assert!(!payload.payload_released());
        drop(actual);
        assert!(payload.payload_released() && receipt.is_released());
        assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
    }
}

#[test]
fn six_inline_controls_at_start_middle_and_end_never_receive_absence_certificate() {
    // All six existing Markdown controls defeat an absence certificate regardless of
    // position, while independent UBA results must remain complete and unchanged.
    for scalar in ['\t', '\n', '\r', '\u{0085}', '\u{2028}', '\u{2029}'] {
        for text in [
            format!("{scalar}中文"),
            format!("中{scalar}文"),
            format!("中文{scalar}"),
        ] {
            for base in [BidiBaseDirection::Auto, BidiBaseDirection::LeftToRight] {
                let (_directory, document) = fixture(&text);
                let source = MarkdownLayoutTextSource::capture(&document);
                let authority = source.authority();
                let (mut actual, _) =
                    prepared(source.clone(), base, &MarkdownBidiCancellation::new());
                assert!(
                    actual
                        .uniform_control_free_span(&authority, 0..source.utf16_len())
                        .unwrap()
                        .is_none()
                );
                assert_oracle(&text, base, &mut actual);
                let synchronous = MarkdownBidiParagraph::analyze_layout_source(
                    source.clone(),
                    &authority,
                    0..source.utf16_len(),
                    base,
                    BidiMemoryBudget::new(65536).unwrap(),
                )
                .unwrap();
                assert!(
                    synchronous
                        .uniform_control_free_span(&authority, 0..source.utf16_len())
                        .unwrap()
                        .is_none()
                );
            }
        }
    }
    for text in ["中文\r\n", "中\r\n文"] {
        let (_directory, document) = fixture(text);
        let source = MarkdownLayoutTextSource::capture(&document);
        let authority = source.authority();
        let (mut actual, _) = prepared(
            source.clone(),
            BidiBaseDirection::Auto,
            &MarkdownBidiCancellation::new(),
        );
        assert!(
            actual
                .uniform_control_free_span(&authority, 0..source.utf16_len())
                .unwrap()
                .is_none()
        );
        assert_oracle(text, BidiBaseDirection::Auto, &mut actual);
    }
}

#[test]
fn other_unicode_controls_do_not_expand_the_existing_markdown_control_set() {
    for scalar in ['\0', '\u{000b}', '\u{000c}', '\u{001f}'] {
        let text = format!("中文{scalar}");
        let (_directory, document) = fixture(&text);
        let source = MarkdownLayoutTextSource::capture(&document);
        let authority = source.authority();
        let (mut actual, _) = prepared(
            source.clone(),
            BidiBaseDirection::Auto,
            &MarkdownBidiCancellation::new(),
        );
        assert!(
            actual
                .uniform_control_free_span(&authority, 0..source.utf16_len())
                .unwrap()
                .is_some()
        );
        assert_oracle(&text, BidiBaseDirection::Auto, &mut actual);
    } // This fact excludes only the product's six existing controls, without expanding interaction policy to all Unicode controls.
}

#[test]
fn paged_and_rtl_negative_paths_preserve_complete_original_uba_results() {
    // Ineligible direction/classes retain full Paged UBA. Proven multi-B level zero
    // still preserves each actual paragraph and grants no whole-source control-free span.
    for (text, base) in [
        ("中文", BidiBaseDirection::RightToLeft),
        ("中文א", BidiBaseDirection::Auto),
        ("中\u{202b}a\u{202c}文", BidiBaseDirection::Auto),
        ("中\u{2067}א\u{2069}文", BidiBaseDirection::Auto),
        ("中\n文", BidiBaseDirection::Auto),
    ] {
        let (_directory, document) = fixture(text);
        let source = MarkdownLayoutTextSource::capture(&document);
        let authority = source.authority();
        let (mut actual, _) = prepared(source.clone(), base, &MarkdownBidiCancellation::new());
        if text == "中\n文" {
            assert!(matches!(actual.resolution, Resolution::UniformZero(_)));
            assert_eq!(actual.paragraph_range_at_utf16(0).unwrap(), Some(0..2));
            assert_eq!(actual.paragraph_range_at_utf16(2).unwrap(), Some(2..3));
        } else {
            assert!(matches!(actual.resolution, Resolution::Paged(_)));
        }
        assert!(
            actual
                .uniform_control_free_span(&authority, 0..source.utf16_len())
                .unwrap()
                .is_none()
        );
        assert_oracle(text, base, &mut actual);
    }
}

#[test]
fn partial_snapshot_cannot_claim_source_and_bound_partial_source_cannot_extend_coverage() {
    // A Snapshot-only analysis has no captured layout-source authority. Binding the
    // actual partial source permits only its original analyzed interval.
    let (_directory, document) = fixture("AA中文😀BB");
    let source = MarkdownLayoutTextSource::capture(&document);
    let authority = source.authority();
    let snapshot_only = MarkdownBidiParagraph::analyze(
        document.text_snapshot(),
        2..6,
        BidiBaseDirection::Auto,
        BidiMemoryBudget::new(65536).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        snapshot_only.uniform_control_free_span(&authority, 2..6),
        Err(BidiError::SourceMismatch)
    ));
    let actual = MarkdownBidiParagraph::analyze_layout_source(
        source.clone(),
        &authority,
        2..6,
        BidiBaseDirection::Auto,
        BidiMemoryBudget::new(65536).unwrap(),
    )
    .unwrap();
    assert_eq!(
        actual
            .uniform_control_free_span(&authority, 2..6)
            .unwrap()
            .unwrap()
            .source_range(),
        2..6
    );
    // A level/control fact may cover numeric low-surrogate coordinates without
    // certifying them as editable positions or GCB endpoints.
    assert!(
        actual
            .uniform_control_free_span(&authority, 5..6)
            .unwrap()
            .is_some()
    );
    assert!(matches!(
        actual.uniform_control_free_span(&authority, 1..6),
        Err(BidiError::InvalidRange)
    ));
    assert!(matches!(
        actual.uniform_control_free_span(&authority, 2..7),
        Err(BidiError::InvalidRange)
    ));
    let foreign = MarkdownLayoutTextSource::capture(&document);
    assert!(matches!(
        actual.uniform_control_free_span(&foreign.authority(), 2..6),
        Err(BidiError::SourceMismatch)
    ));
}

/// When partial analysis starts inside a B paragraph, the Snapshot cursor's preceding
/// text is outside the analyzed authority. Exclude the actual trailing control and
/// revoke the completed body span after cancellation.
#[test]
fn scanned_partial_terminal_b_keeps_its_original_analysis_start_and_body_end() {
    for ending in ['\n', '\r', '\u{2029}'] {
        let text = format!("前缀中文😀{ending}后段");
        let (directory, document) = fixture(&text);
        let source = MarkdownLayoutTextSource::capture(&document);
        let authority = source.authority();
        let start = "前缀".encode_utf16().count();
        let content_end = "前缀中文😀".encode_utf16().count();
        let end = content_end + ending.len_utf16();
        let mapping = source.original_backing_observer().unwrap();
        let payload = source.resource_observer();
        let cancel = MarkdownBidiCancellation::new();
        let mut actual = MarkdownBidiParagraph::analyze_layout_source_cancellable(
            source.clone(),
            &authority,
            start..end,
            BidiBaseDirection::Auto,
            BidiMemoryBudget::new(65536).unwrap(),
            &cancel,
        )
        .unwrap();
        let receipt = actual.resource_observer();
        assert!(
            actual
                .uniform_control_free_span(&authority, start..content_end)
                .unwrap()
                .is_none()
        );
        let mut ready = false;
        for _ in 0..100_000 {
            let step = actual
                .prepare_uniform_paragraph_range(&authority, start, ONE)
                .unwrap();
            assert!(step.bytes <= 1 && step.scalars <= 1 && step.metadata <= 1);
            assert!(step.maximum_borrow <= 1);
            if step.ready {
                ready = true;
                break;
            }
        }
        assert!(ready);
        let span = actual
            .uniform_control_free_span(&authority, start..content_end)
            .unwrap()
            .unwrap();
        assert_eq!(span.source_range(), start..content_end);
        assert_eq!(span.paragraph_range(), start..end);
        assert_eq!(span.content_end(), content_end);
        assert_eq!(
            actual.paragraph_range_at_utf16(start).unwrap(),
            Some(start..end)
        );
        assert!(
            actual
                .uniform_control_free_span(&authority, start..end)
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            actual.uniform_control_free_span(&authority, 0..start),
            Err(BidiError::InvalidRange)
        ));
        cancel.cancel();
        assert!(matches!(
            actual.uniform_control_free_span(&authority, start..content_end),
            Err(BidiError::Cancelled)
        ));
        assert_eq!(
            std::fs::read(directory.path().join("uniform-control.md")).unwrap(),
            text.as_bytes()
        );
        drop((actual, source, document));
        assert!(payload.payload_released() && receipt.is_released());
        assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
    }
}

/// Table flow prepares actual B paragraphs: a trailing separator is outside the body,
/// while internal TAB/LS remains inside it. One-unit quanta exercise the real Visible
/// consumer without substituting certificate fields or fabricated paragraph ranges.
#[cfg(feature = "s2-table-window-prototype")]
#[test]
fn visible_table_flow_keeps_terminal_b_outside_the_actual_control_free_body() {
    for ending in ["\n", "\r", "\u{2029}", "\r\n"] {
        for interior in ["", "\t", "\u{2028}"] {
            let prefix = "前段\n";
            let content = format!("中文{interior}😀");
            let text = format!("{prefix}{content}{ending}后段\n");
            let start = prefix.encode_utf16().count();
            let content_end = start + content.encode_utf16().count();
            let (directory, document) = fixture(&text);
            let source = MarkdownLayoutTextSource::capture(&document);
            let authority = source.authority();
            let mapping = source.original_backing_observer().unwrap();
            let payload = source.resource_observer();
            let writer = crate::markdown_layout_source::table_rows::TablePlanWriter::new(&source);
            let cancel = MarkdownBidiCancellation::new();
            let (job, receipt) = MarkdownBidiPrepareJob::new_layout_source_table_observed(
                source.clone(),
                &authority,
                writer.plan(),
                None,
                BidiBaseDirection::Auto,
                BidiMemoryBudget::new(65536).unwrap(),
                &cancel,
            );
            let mut job = job.unwrap();
            assert_eq!(
                job.step(&authority, ONE).unwrap().status,
                MarkdownBidiPrepareStatus::Ready
            );
            let mut actual = job.take_ready(&authority).unwrap().unwrap();
            drop(job);
            let mut work = crate::markdown_bidi::visible::VisiblePreparation::default();
            let mut ready = false;
            for _ in 0..100_000 {
                let step = actual
                    .prepare_layout_paragraph_range(&authority, start, ONE, &mut work)
                    .unwrap();
                assert!(step.bytes <= 1 && step.scalars <= 1 && step.metadata <= 1);
                assert!(step.maximum_borrow <= 1);
                if step.ready {
                    ready = true;
                    break;
                }
                // Open Script consumes no control-free span until both complete UBA
                // and the actual body endpoints have been prepared.
                assert!(matches!(
                    actual.uniform_control_free_span(&authority, start..content_end),
                    Ok(None) | Err(BidiError::UnpreparedRange)
                ));
            }
            assert!(
                ready,
                "真实Visible正文准备未完成：ending={ending:?} interior={interior:?}"
            );
            let span = actual
                .uniform_control_free_span(&authority, start..content_end)
                .unwrap();
            if interior.is_empty() {
                let span = span.expect("真实末尾B之外的正文应可开放Script前缀");
                assert_eq!(span.source_range(), start..content_end);
                assert_eq!(span.paragraph_range(), start..content_end + 1);
                assert_eq!(span.content_end(), content_end);
                assert_eq!(span.level(), 0);
            } else {
                assert!(span.is_none(), "同B段内部TAB/LS不能取得无控制授权");
            }
            assert!(
                actual
                    .uniform_control_free_span(&authority, start..content_end + 1)
                    .unwrap()
                    .is_none()
            );
            // CRLF is one GCB but two actual B paragraphs. The following LF cannot
            // become body text or extend the preceding paragraph's authority.
            if ending == "\r\n" {
                let next = content_end + 1;
                let mut complete = false;
                for _ in 0..100_000 {
                    let step = actual
                        .prepare_layout_paragraph_range(&authority, next, ONE, &mut work)
                        .unwrap();
                    assert!(step.bytes <= 1 && step.scalars <= 1 && step.metadata <= 1);
                    assert!(step.maximum_borrow <= 1);
                    if step.ready {
                        complete = true;
                        break;
                    }
                }
                assert!(complete);
                assert_eq!(
                    actual.paragraph_range_at_utf16(next).unwrap(),
                    Some(next..next + 1)
                );
                assert!(
                    actual
                        .uniform_control_free_span(&authority, next..next + 1)
                        .unwrap()
                        .is_none()
                );
            }
            let analyzed = format!("{content}{}", ending.chars().next().unwrap());
            let units: Vec<u16> = analyzed.encode_utf16().collect();
            let expected = BidiInfo::new_with_data_source(&UnicodeBidiData, &units, None);
            for (offset, level) in expected.levels.iter().enumerate() {
                assert_eq!(
                    actual.level_at_utf16(start + offset).unwrap(),
                    level.number()
                );
            }
            // Cancellation revokes the same Source's completed span. Preparation has
            // changed neither text/history nor the isolated file's bytes.
            cancel.cancel();
            assert!(matches!(
                actual.uniform_control_free_span(&authority, start..content_end),
                Err(BidiError::Cancelled)
            ));
            assert_eq!(
                std::fs::read(directory.path().join("uniform-control.md")).unwrap(),
                text.as_bytes()
            );
            drop((work, actual, writer, source, document));
            assert!(payload.payload_released() && receipt.is_released());
            assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
        }
    }
}

/// The actual table-flow Composer must issue its first bounded style request before
/// completing SCX for a long Chinese B paragraph. This checks the original open entry,
/// not guessed fonts or LineReady; existing font/glyph regressions own later shaping.
#[cfg(feature = "s2-table-window-prototype")]
#[test]
fn table_flow_terminal_b_opens_first_style_before_the_complete_long_scx() {
    let content = "巨段落可见区域绘制检查 ".repeat(1500);
    let text = format!("{content}\n后继\n");
    let (directory, document) = fixture(&text);
    let source = MarkdownLayoutTextSource::capture(&document);
    let authority = source.authority();
    let mapping = source.original_backing_observer().unwrap();
    let payload = source.resource_observer();
    let mut worker = crate::MarkdownLineComposer::new_with_table_flow(
        source.clone(),
        &authority,
        crate::MarkdownLineFontCatalog::new(),
        200 * 64,
        crate::MarkdownLineComposeCancellation::new(),
    )
    .unwrap();
    let resources = worker.worker_resource_observer();
    let budget = crate::MarkdownLineComposeBudget {
        bytes: 1,
        scalars: 1,
        records: 1,
        metadata_units: 1,
    };
    let mut requested = false;
    for _ in 0..200_000 {
        let step = worker.step(budget).unwrap();
        assert!(
            step.bytes <= 1 && step.scalars <= 1 && step.records <= 1 && step.metadata_units <= 1
        );
        match step.answer {
            crate::MarkdownLineComposeAnswer::Yielded => {}
            crate::MarkdownLineComposeAnswer::NeedStyle(request) => {
                assert!(request.source() == &authority, "首个请求必须属于原来源");
                assert_eq!(request.source_range().start, 0);
                assert!(request.source_range().end < content.encode_utf16().count());
                requested = true;
                break;
            }
            _ => panic!("长中文正文首个真实样式请求之前出现其它结果"),
        }
    }
    assert!(requested, "实际table-flow未交付正文首个有界样式请求");
    assert_eq!(
        std::fs::read(directory.path().join("uniform-control.md")).unwrap(),
        text.as_bytes()
    );
    drop((worker, source, document));
    assert!(resources.is_released() && payload.payload_released());
    assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
}

#[test]
fn subrange_span_returns_original_analyzed_paragraph_without_reborrowing_source() {
    // Subrange facts retain the original analyzed paragraph and require no source
    // reborrow after actual read-window reclaim.
    let (_directory, document) = fixture("AA中文😀BB");
    let source = MarkdownLayoutTextSource::capture(&document);
    let authority = source.authority();
    let mapping = source.original_backing_observer().unwrap();
    let source_receipt = source.resource_observer();
    let actual = MarkdownBidiParagraph::analyze_layout_source(
        source.clone(),
        &authority,
        2..6,
        BidiBaseDirection::Auto,
        BidiMemoryBudget::new(65536).unwrap(),
    )
    .unwrap();
    document.reclaim_original_read_windows_for_test().unwrap();
    let before = (mapping.managed_resident_bytes(), mapping.reclaim_attempts());
    for query in [3..4, 5..6] {
        let span = actual
            .uniform_control_free_span(&authority, query.clone())
            .unwrap()
            .unwrap();
        assert_eq!(span.source_range(), query);
        assert_eq!(span.paragraph_range(), 2..6); // One-unit and low-surrogate queries neither shrink nor extend the analyzed paragraph.
        assert_eq!(span.level(), 0);
    }
    assert_eq!(
        (mapping.managed_resident_bytes(), mapping.reclaim_attempts()),
        before
    );
    drop((actual, source, document));
    assert!(source_receipt.payload_released());
    assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
}

#[test]
fn frozen_edit_source_and_sync_prepare_share_only_their_original_certificate() {
    // Cooperative and synchronous preparation agree for the same capture. A later
    // edit keeps the old immutable fact valid but cannot borrow its old authority.
    let (_directory, mut document) = fixture("中文😀abc");
    let source = MarkdownLayoutTextSource::capture(&document);
    let authority = source.authority();
    let (actual, _) = prepared(
        source.clone(),
        BidiBaseDirection::Auto,
        &MarkdownBidiCancellation::new(),
    );
    let synchronous = MarkdownBidiParagraph::analyze_layout_source(
        source.clone(),
        &authority,
        0..source.utf16_len(),
        BidiBaseDirection::Auto,
        BidiMemoryBudget::new(65536).unwrap(),
    )
    .unwrap();
    assert!(
        synchronous
            .uniform_control_free_span(&authority, 0..source.utf16_len())
            .unwrap()
            .is_some()
    );
    document
        .insert_utf16(0, "\t", MarkdownTextSelection::caret(1))
        .unwrap();
    let edited = MarkdownLayoutTextSource::capture(&document);
    let edited_authority = edited.authority();
    assert!(
        actual
            .uniform_control_free_span(&authority, 0..source.utf16_len())
            .unwrap()
            .is_some()
    );
    assert!(matches!(
        actual.uniform_control_free_span(&edited_authority, 0..source.utf16_len()),
        Err(BidiError::SourceMismatch)
    ));
    let edited_actual = MarkdownBidiParagraph::analyze_layout_source(
        edited.clone(),
        &edited_authority,
        0..edited.utf16_len(),
        BidiBaseDirection::Auto,
        BidiMemoryBudget::new(65536).unwrap(),
    )
    .unwrap();
    assert!(
        edited_actual
            .uniform_control_free_span(&edited_authority, 0..edited.utf16_len())
            .unwrap()
            .is_none()
    );
}

#[test]
fn real_source_eio_precedes_foreign_authority_cancel_and_ready_control_proof() {
    // Fail actual original-window reclaim after successful preparation. Its first EIO
    // must precede cancellation, foreign authority, and a previously reusable span.
    let (_directory, document) = fixture("中文😀");
    let source = MarkdownLayoutTextSource::capture(&document);
    let authority = source.authority();
    let mapping = source.original_backing_observer().unwrap();
    let payload = source.resource_observer();
    let cancel = MarkdownBidiCancellation::new();
    let (actual, _) = prepared(source.clone(), BidiBaseDirection::Auto, &cancel);
    assert!(
        actual
            .uniform_control_free_span(&authority, 0..source.utf16_len())
            .unwrap()
            .is_some()
    );
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
        actual.uniform_control_free_span(&foreign, 0..source.utf16_len()),
        Err(BidiError::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
    let receipt = actual.resource_observer();
    drop(source);
    drop(document);
    drop(actual);
    assert!(payload.payload_released() && receipt.is_released());
    assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0);
    assert_eq!(mapping.first_errno(), Some(libc::EIO));
}

#[test]
fn pending_and_cancelled_uniform_never_publish_a_reusable_control_fact() {
    // Whole-root UBA Ready and actual paragraph-body preparation are independent stages.
    // Cancellation must revoke both before an unfinished body can publish a reusable span.
    let (_directory, document) = fixture("中文😀\n");
    let source = MarkdownLayoutTextSource::capture(&document);
    let authority = source.authority();
    let cancel = MarkdownBidiCancellation::new();
    let mut pending = MarkdownBidiPrepareJob::new_layout_source(
        source.clone(),
        &authority,
        BidiBaseDirection::Auto,
        BidiMemoryBudget::new(65536).unwrap(),
        &cancel,
    )
    .unwrap();
    assert_eq!(
        pending.step(&authority, ONE).unwrap().status,
        MarkdownBidiPrepareStatus::Ready
    );
    let mut paragraph = pending.take_ready(&authority).unwrap().unwrap();
    assert!(
        !paragraph
            .prepare_uniform_paragraph_range(&authority, 0, ONE)
            .unwrap()
            .ready
    );
    assert!(
        paragraph
            .uniform_control_free_span(&authority, 0..1)
            .unwrap()
            .is_none()
    ); // Complete root level zero does not supply missing B endpoints or the current control-free body range.
    cancel.cancel();
    assert!(matches!(
        paragraph.prepare_uniform_paragraph_range(&authority, 0, ONE),
        Err(BidiError::Cancelled)
    ));
    assert!(matches!(
        paragraph.uniform_control_free_span(&authority, 0..1),
        Err(BidiError::Cancelled)
    ));
    assert!(matches!(
        pending.step(&authority, ONE),
        Err(BidiError::Cancelled)
    ));
    assert!(matches!(
        pending.take_ready(&authority),
        Err(BidiError::Cancelled)
    ));
    let ready_cancel = MarkdownBidiCancellation::new();
    let (actual, _) = prepared(source.clone(), BidiBaseDirection::Auto, &ready_cancel);
    ready_cancel.cancel();
    assert!(matches!(
        actual.uniform_control_free_span(&authority, 0..source.utf16_len()),
        Err(BidiError::Cancelled)
    ));
}
