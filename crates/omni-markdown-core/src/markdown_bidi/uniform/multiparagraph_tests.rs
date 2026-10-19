//! 原Source多段level0只按需定位真实B端点；字节、scalar、目录预算与独立完整UBA语义分别核验。

use super::*;
use crate::{MarkdownDocumentBuffer, MarkdownLayoutTextSource};
use std::io::Write;

fn fixture(text: &str) -> (tempfile::TempDir, MarkdownDocumentBuffer) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("bidi-paragraph.md");
    std::fs::write(&path, text).unwrap();
    let document = MarkdownDocumentBuffer::open(&path, 8 * 1024 * 1024).unwrap();
    (directory, document)
}

fn ready(
    source: &MarkdownLayoutTextSource,
    cancel: &MarkdownBidiCancellation,
) -> MarkdownBidiParagraph {
    let authority = source.authority();
    let mut job = MarkdownBidiPrepareJob::new_layout_source(
        source.clone(),
        &authority,
        BidiBaseDirection::Auto,
        BidiMemoryBudget::new(65536).unwrap(),
        cancel,
    )
    .unwrap();
    let step = job
        .step(
            &authority,
            MarkdownBidiPrepareBudget {
                bytes: 1,
                scalars: 1,
                metadata_ops: 1,
            },
        )
        .unwrap();
    assert_eq!(step.status, MarkdownBidiPrepareStatus::Ready);
    assert_eq!((step.bytes, step.scalars, step.metadata_ops), (0, 0, 0));
    let value = job.take_ready(&authority).unwrap().unwrap();
    assert!(value.uses_prepared_paragraphs());
    assert_eq!(value.source_range(), 0..source.utf16_len());
    value
}

fn prepare(
    value: &mut MarkdownBidiParagraph,
    source: &MarkdownLayoutTextSource,
    position: usize,
    budget: MarkdownBidiPrepareBudget,
) {
    loop {
        let step = value
            .prepare_uniform_paragraph_range(&source.authority(), position, budget)
            .unwrap();
        assert!(
            step.bytes <= budget.bytes
                && step.scalars <= budget.scalars
                && step.metadata <= budget.metadata_ops
        );
        assert!(step.maximum_borrow <= budget.bytes.min(4096));
        if step.ready {
            break;
        }
    } // 等待的是这个真实游标完成；没有轮询Native、增加预算或把Pending当已找到的段。
}

#[test]
fn exact_cr_lf_nel_ps_and_ascii_b_separators_keep_full_unicode_oracle() {
    for text in [
        "alpha\r\nsecond\r\n中文😀e\u{301}",
        "\r\n\r\n",
        "a\u{85}b\u{2029}c\u{1c}d\u{1d}e\u{1e}尾",
        "a\u{000b}\u{000c}\u{00ad}\u{0080}\u{001f}\n中",
    ] {
        let (_directory, document) = fixture(text);
        let mapping = document.original_backing_observer().unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let payload = source.resource_observer();
        let mut actual = ready(&source, &MarkdownBidiCancellation::new());
        let resources = actual.resource_observer();
        let units: Vec<_> = text.encode_utf16().collect();
        let oracle =
            unicode_bidi::utf16::BidiInfo::new_with_data_source(&UnicodeBidiData, &units, None);
        for budget in [
            MarkdownBidiPrepareBudget {
                bytes: 1,
                scalars: 1,
                metadata_ops: 1,
            },
            MarkdownBidiPrepareBudget {
                bytes: 7,
                scalars: 3,
                metadata_ops: 2,
            },
        ] {
            let mut offset = 0;
            for scalar in text.chars() {
                prepare(&mut actual, &source, offset, budget);
                let expected = oracle
                    .paragraphs
                    .iter()
                    .find(|paragraph| paragraph.range.contains(&offset))
                    .unwrap()
                    .range
                    .clone();
                assert_eq!(
                    actual.paragraph_range_at_utf16(offset).unwrap(),
                    Some(expected)
                );
                assert_eq!(
                    actual.paragraph_base_level_at_utf16(offset).unwrap(),
                    Some(0)
                );
                offset += scalar.len_utf16();
            }
            assert_eq!(actual.paragraph_range_at_utf16(offset).unwrap(), None);
        }
        control_tests::assert_oracle(text, BidiBaseDirection::Auto, &mut actual);
        assert!(!resources.snapshot().file_created && resources.snapshot().cache_bytes == 0);
        drop((actual, source, document));
        assert!(payload.payload_released() && resources.is_released());
        assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
    }
}

#[test]
fn mixed_leaf_scalar_endpoints_foreign_source_cancel_and_actual_eio_keep_priority() {
    let (_directory, document) = fixture("a😀b\r\n中\u{85}尾");
    let source = MarkdownLayoutTextSource::capture(&document);
    let foreign = MarkdownLayoutTextSource::capture(&document);
    let cancel = MarkdownBidiCancellation::new();
    let mut actual = ready(&source, &cancel);
    let one = MarkdownBidiPrepareBudget {
        bytes: 1,
        scalars: 1,
        metadata_ops: 1,
    };
    assert!(matches!(
        actual.prepare_uniform_paragraph_range(&foreign.authority(), 1, one),
        Err(BidiError::SourceMismatch)
    ));
    let error = loop {
        match actual.prepare_uniform_paragraph_range(&source.authority(), 2, one) {
            Err(error) => break error,
            Ok(step) => assert!(!step.ready),
        }
    };
    assert!(matches!(
        error,
        BidiError::Source(MarkdownDocumentBufferError::NotUnicodeBoundary)
    ));
    prepare(&mut actual, &source, 1, one);
    assert_eq!(actual.paragraph_range_at_utf16(1).unwrap(), Some(0..5));
    // 真实借用与驱逐失败在取消之前发生；后续查询不能把原errno重写为Cancelled或foreign。
    source.visit_byte_range(0..1, |_| Ok(())).unwrap();
    document.fail_next_original_reclaim_for_test();
    assert_eq!(
        document.reclaim_original_read_windows_for_test(),
        Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
            libc::EIO
        ))
    );
    cancel.cancel();
    assert!(matches!(
        actual.prepare_uniform_paragraph_range(&foreign.authority(), 5, one),
        Err(BidiError::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
    let mapping = document.original_backing_observer().unwrap();
    let payload = source.resource_observer();
    drop((actual, source, foreign, document));
    assert!(payload.payload_released() && mapping.unmap_succeeded() && mapping.mapped_bytes() == 0);
    assert_eq!(mapping.first_errno(), Some(libc::EIO));
    let (_directory, document) = fixture("a\r\nb");
    let source = MarkdownLayoutTextSource::capture(&document);
    let cancel = MarkdownBidiCancellation::new();
    let mut actual = ready(&source, &cancel);
    let _ = actual
        .prepare_uniform_paragraph_range(&source.authority(), 0, one)
        .unwrap();
    cancel.cancel();
    assert!(matches!(
        actual.prepare_uniform_paragraph_range(&source.authority(), 0, one),
        Err(BidiError::Cancelled)
    ));
}

#[test]
fn late_rtl_an_isolate_and_explicit_classes_never_receive_global_zero_level_proof() {
    for suffix in ["א", "ش", "١", "\u{2067}א\u{2069}", "\u{202b}a\u{202c}"] {
        let text = format!("first\r\n{}{suffix}", "a".repeat(120));
        let (_directory, document) = fixture(&text);
        let source = MarkdownLayoutTextSource::capture(&document);
        let authority = source.authority();
        let mut job = MarkdownBidiPrepareJob::new_layout_source(
            source,
            &authority,
            BidiBaseDirection::Auto,
            BidiMemoryBudget::new(65536).unwrap(),
            &MarkdownBidiCancellation::new(),
        )
        .unwrap();
        let mut actual = loop {
            let step = job
                .step(
                    &authority,
                    MarkdownBidiPrepareBudget {
                        bytes: 7,
                        scalars: 3,
                        metadata_ops: 2,
                    },
                )
                .unwrap();
            assert!(step.bytes <= 7 && step.scalars <= 3 && step.metadata_ops <= 2);
            if step.status == MarkdownBidiPrepareStatus::Ready {
                break job.take_ready(&authority).unwrap().unwrap();
            }
        };
        assert!(!actual.uses_prepared_paragraphs());
        control_tests::assert_oracle(&text, BidiBaseDirection::Auto, &mut actual);
    }
}

#[test]
fn four_mebibyte_mixed_span_queries_only_true_b_leaves_and_releases_original_source() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sparse-b.md");
    let mut file = std::fs::File::create(&path).unwrap();
    let block = [b'a'; 4096];
    for _ in 0..1024 {
        file.write_all(&block).unwrap();
    }
    file.write_all(b"\r\nlast").unwrap();
    file.flush().unwrap();
    drop(file);
    let mut document = MarkdownDocumentBuffer::open(&path, 8 * 1024 * 1024).unwrap();
    document
        .insert_utf16(0, "first\r\n", crate::MarkdownTextSelection::caret(7))
        .unwrap();
    // 在原Span内部真实插入控制，后半Span保留非零目录起点；不能用完整block才正确的prefix求和。
    let middle = 10 * 4096 + 9;
    document
        .insert_utf16(
            middle,
            "\r\n",
            crate::MarkdownTextSelection::caret(middle + 2),
        )
        .unwrap();
    let mapping = document.original_backing_observer().unwrap();
    let source = MarkdownLayoutTextSource::capture(&document);
    let payload = source.resource_observer();
    let mut value = ready(&source, &MarkdownBidiCancellation::new());
    let budget = MarkdownBidiPrepareBudget {
        bytes: 4096,
        scalars: 256,
        metadata_ops: 256,
    };
    for (position, expected) in [
        (0, 0..6),
        (6, 6..7),
        (7, 7..middle + 1),
        (middle + 1, middle + 1..middle + 2),
        (middle + 2, middle + 2..7 + 4 * 1024 * 1024 + 3),
    ] {
        let mut bytes = 0;
        loop {
            let step = value
                .prepare_uniform_paragraph_range(&source.authority(), position, budget)
                .unwrap();
            assert!(step.bytes <= 4096 && step.scalars <= 256 && step.metadata <= 256);
            bytes += step.bytes;
            if step.ready {
                break;
            }
        }
        assert_eq!(
            value.paragraph_range_at_utf16(position).unwrap(),
            Some(expected)
        );
        assert!(
            bytes < 32 * 1024,
            "端点查询只能读取有限命中叶，不能顺扫四MiB单段"
        );
    }
    let resources = value.resource_observer();
    assert!(!resources.snapshot().file_created && resources.snapshot().cache_bytes == 0);
    drop((value, source, document));
    assert!(payload.payload_released() && resources.is_released());
    assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
}
