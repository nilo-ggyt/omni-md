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

/// emoji 使全源整数端点证明失效，但同段逐 GCB 定位仍应只顺读新标量。
/// 实际源跨过原 mmap 叶，分别用一字节和生产量子核结果、收费与完整释放。
#[test]
fn mixed_emoji_forward_graphemes_reuse_only_certified_scalar_points() {
    let prefix = "前段\n";
    let unit = "中😀e\u{301}文";
    let body = unit.repeat(384);
    let text = format!("{prefix}{body}\n后段😀\n");
    let start = prefix.encode_utf16().count();
    let content_end = start + body.encode_utf16().count();
    let expected = start..content_end + 1;
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
            bytes: 4096,
            scalars: 256,
            metadata_ops: 256,
        },
    ] {
        let (directory, document) = fixture(&text);
        let source = MarkdownLayoutTextSource::capture(&document);
        let mapping = source.original_backing_observer().unwrap();
        let payload = source.resource_observer();
        let authority = source.authority();
        let cancel = MarkdownBidiCancellation::new();
        let mut actual = ready(&source, &cancel);
        let resources = actual.resource_observer();
        let mut total_bytes = 0;
        let mut total_scalars = 0;
        // 单元的四个真实 GCB 起点分别是中、emoji、e+组合音标、文；不把低代理项
        // 或组合符内部当正常请求。跨叶后仍必须由同 Frozen 的真实 Boundary 验证。
        for repetition in 0..384 {
            for relative in [0, 1, 3, 5] {
                let position = start + repetition * unit.encode_utf16().count() + relative;
                let mut complete = false;
                for _ in 0..100_000 {
                    let step = actual
                        .prepare_uniform_paragraph_range(&authority, position, budget)
                        .unwrap();
                    assert!(step.bytes <= budget.bytes && step.scalars <= budget.scalars);
                    assert!(step.metadata <= budget.metadata_ops);
                    assert!(step.maximum_borrow <= budget.bytes.min(4096));
                    total_bytes += step.bytes;
                    total_scalars += step.scalars;
                    if step.ready {
                        complete = true;
                        break;
                    }
                }
                assert!(complete, "原量子内未完成有限真实段定位");
                let independent = oracle
                    .paragraphs
                    .iter()
                    .find(|paragraph| paragraph.range.contains(&position))
                    .unwrap();
                assert_eq!(independent.range, expected);
                assert_eq!(
                    actual.paragraph_range_at_utf16(position).unwrap(),
                    Some(independent.range.clone())
                );
            }
        }
        // 这核验实际 lease/解码工作，防止每个 GCB 重扫同一叶或反复查 B 两端；
        // 保留冷首查、跨叶与 UTF8 预读余量，不依赖游标内部字段或调用次数。
        assert!(total_bytes < text.len() * 8, "同段前进重复读取正文");
        assert!(total_scalars < text.chars().count() * 8);
        // 即使另一 capture 来自同一文档，它也不拥有已保留的 Frozen 点。
        let foreign = MarkdownLayoutTextSource::capture(&document);
        assert!(matches!(
            actual.prepare_uniform_paragraph_range(&foreign.authority(), content_end, budget),
            Err(BidiError::SourceMismatch)
        ));
        drop(foreign);
        let span = actual
            .uniform_control_free_span(&authority, start..content_end)
            .unwrap()
            .unwrap();
        assert_eq!(span.paragraph_range(), expected);
        assert_eq!(span.content_end(), content_end);

        // 后退重新走冷路径；随后只前进一步到 emoji 开头，再请求其低代理项。
        // 已知段范围与旧成功点都不能把这个无效位置变成 Ready。
        prepare(&mut actual, &source, start, budget);
        prepare(&mut actual, &source, start + 1, budget);
        let error = loop {
            match actual.prepare_uniform_paragraph_range(&authority, start + 2, budget) {
                Err(error) => break error,
                Ok(step) => {
                    assert!(!step.ready);
                    assert!(step.bytes <= budget.bytes && step.scalars <= budget.scalars);
                    assert!(step.metadata <= budget.metadata_ops);
                }
            }
        };
        assert!(matches!(
            error,
            BidiError::Source(MarkdownDocumentBufferError::NotUnicodeBoundary)
        ));
        prepare(&mut actual, &source, start + 3, budget);
        assert_eq!(
            actual.paragraph_range_at_utf16(start + 3).unwrap(),
            Some(expected.clone())
        );
        // 另一真实 B 段不是原范围的续体，段尾换行仍属于原 B 范围而不是正文。
        prepare(&mut actual, &source, content_end + 1, budget);
        assert_eq!(
            actual.paragraph_range_at_utf16(content_end + 1).unwrap(),
            Some(content_end + 1..source.utf16_len())
        );
        cancel.cancel();
        assert!(matches!(
            actual.prepare_uniform_paragraph_range(&authority, content_end + 2, budget),
            Err(BidiError::Cancelled)
        ));
        assert_eq!(
            std::fs::read(directory.path().join("bidi-paragraph.md")).unwrap(),
            text.as_bytes()
        );
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
    prepare(&mut actual, &source, 3, one);
    assert_eq!(actual.paragraph_range_at_utf16(3).unwrap(), Some(0..5));
    // 此时已经走过同段 emoji 的真实前进快路；保留点也不能绕过后续首个 Source 错误。
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
