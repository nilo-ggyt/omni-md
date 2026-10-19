//! 真实规范树、紧凑原始目录和历史根的属性差分；不以派生布局结果倒推文档事实。

use super::super::*;
use super::*;

fn fixture(text: &str) -> (tempfile::TempDir, MarkdownDocumentBuffer) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("unicode-properties.md");
    std::fs::write(&path, text).unwrap();
    let document = MarkdownDocumentBuffer::open(&path, 8 * 1024 * 1024).unwrap();
    (directory, document)
}

fn oracle(text: &str) -> MarkdownUnicodeProperties {
    let mut classes = 0;
    let mut separators = 0;
    let mut minimum = 0_u8;
    let mut maximum = 0_u8;
    for scalar in text.chars() {
        // 独立迭代只在有限测试oracle执行，生产值必须来自原叶的同一次建树遍历。
        classes |= MarkdownUnicodeProperties::class_mask(UnicodeBidiData.bidi_class(scalar));
        if scalar.is_ascii() && !scalar.is_ascii_control() {
            let byte = scalar as u8;
            minimum = if minimum == 0 {
                byte
            } else {
                minimum.min(byte)
            };
            maximum = maximum.max(byte);
        } // 独立有限oracle不复用生产observe/合并逻辑；DEL仍是控制，不进入可打印包络。
        for (bit, expected) in ['\t', '\r', '\n', '\u{85}', '\u{2028}', '\u{2029}']
            .into_iter()
            .enumerate()
        {
            if scalar == expected {
                separators |= 1 << bit;
            }
        }
    }
    MarkdownUnicodeProperties {
        bidi_classes: classes,
        separators,
        printable_ascii_min: minimum,
        printable_ascii_max: maximum,
    }
}

#[test]
fn all_real_bidi_classes_have_unique_bits_and_unicode18_properties() {
    use BidiClass::*;
    let samples = [
        ('a', L),
        ('א', R),
        ('ش', AL),
        ('1', EN),
        ('+', ES),
        ('$', ET),
        ('١', AN),
        (',', CS),
        ('\u{301}', NSM),
        ('\u{ad}', BN),
        ('\n', B),
        ('\t', S),
        (' ', WS),
        ('!', ON),
        ('\u{202a}', LRE),
        ('\u{202d}', LRO),
        ('\u{202b}', RLE),
        ('\u{202e}', RLO),
        ('\u{202c}', PDF),
        ('\u{2066}', LRI),
        ('\u{2067}', RLI),
        ('\u{2068}', FSI),
        ('\u{2069}', PDI),
    ];
    let mut bits = 0_u32;
    for (scalar, class) in samples {
        let mask = MarkdownUnicodeProperties::class_mask(class);
        assert_eq!(mask.count_ones(), 1);
        assert_eq!(bits & mask, 0);
        assert_eq!(UnicodeBidiData.bidi_class(scalar), class);
        let actual = MarkdownDocumentMetrics::from_text(&scalar.to_string()).unicode_properties;
        assert_eq!(actual.bidi_classes(), mask);
        bits |= mask;
    }
    assert_eq!(bits, (1 << 23) - 1);
    // 新分配NSM、默认AL、非字符BN和新版L均必须消费与UBA相同的Unicode18增量。
    for (scalar, class) in [
        ('\u{05c8}', NSM),
        ('\u{086b}', AL),
        ('\u{2065}', BN),
        ('\u{1b3a}', L),
        ('\u{1ccfa}', ON),
        ('\u{10ffff}', BN),
    ] {
        assert_eq!(
            MarkdownDocumentMetrics::from_text(&scalar.to_string())
                .unicode_properties
                .bidi_classes(),
            MarkdownUnicodeProperties::class_mask(class),
        );
    }
    for &(first, last, _, current) in crate::unicode_bidi_data::CHANGED_BOUNDARIES {
        // 逐个增量区间的两端必须进入新聚合，邻点仍由同一真实发布表计算，不能只核几个新字。
        for point in [first.saturating_sub(1), first, last, last + 1] {
            if let Some(scalar) = char::from_u32(point) {
                let class = UnicodeBidiData.bidi_class(scalar);
                if point == first || point == last {
                    assert_eq!(class, current);
                }
                assert_eq!(
                    MarkdownDocumentMetrics::from_text(&scalar.to_string())
                        .unicode_properties
                        .bidi_classes(),
                    MarkdownUnicodeProperties::class_mask(class),
                );
            }
        }
    }
}

#[test]
fn all_128_ascii_properties_match_the_fixed_unicode18_provider_and_every_split() {
    assert_eq!(std::mem::size_of_val(&ASCII_BIDI_MASKS), 512);
    assert_eq!(crate::unicode_bidi_data::UNICODE_VERSION, (18, 0, 0));
    for byte in 0_u8..=127 {
        let scalar = char::from(byte);
        let expected = UnicodeBidiData.bidi_class(scalar);
        // 独立provider仍走原发布数据与增量表；测试不能用新ASCII分类器给自己作oracle。
        assert_eq!(ascii_bidi_class(byte), expected, "U+{byte:04X}");
        assert_eq!(
            ASCII_BIDI_MASKS[usize::from(byte)],
            MarkdownUnicodeProperties::class_mask(expected)
        );
        let actual = MarkdownDocumentMetrics::from_text(&scalar.to_string());
        assert_eq!(actual.unicode_properties, oracle(&scalar.to_string()));
    }
    let text: String = (0_u8..=127).map(char::from).collect();
    let full = MarkdownDocumentMetrics::from_text(&text);
    assert_eq!(full.unicode_properties, oracle(&text));
    assert_eq!(full.unicode_properties.separators(), 0b000111); // ASCII只有TAB、CR、LF。
    for split in 0..=text.len() {
        // 原字符/CRLF/word/terminator规则也完整差分，不只比较新class集合。
        let left = MarkdownDocumentMetrics::from_text(&text[..split]);
        let right = MarkdownDocumentMetrics::from_text(&text[split..]);
        assert_eq!(MarkdownDocumentMetrics::joined(left, right), full);
    }
}

#[test]
fn scalar_aggregation_and_every_split_preserve_exact_associative_properties() {
    let text = "中😀e\u{301}\t\r\n\u{85}\u{2028}\u{2029}אش١+,$\u{202a}a\u{202c}\u{2067}א\u{2069}\0\u{b}\u{c}\u{1f}";
    let full = MarkdownDocumentMetrics::from_text(text);
    assert_eq!(full.unicode_properties, oracle(text));
    assert_eq!(full.unicode_properties.separators(), 0x3f);
    let boundaries: Vec<_> = text
        .char_indices()
        .map(|(byte, _)| byte)
        .chain([text.len()])
        .collect();
    for &first in &boundaries {
        let left = MarkdownDocumentMetrics::from_text(&text[..first]);
        let right = MarkdownDocumentMetrics::from_text(&text[first..]);
        assert_eq!(MarkdownDocumentMetrics::joined(left, right), full);
        for &second in boundaries.iter().filter(|&&second| second >= first) {
            // 三段两种结合顺序同时核验原CRLF/word/terminator聚合，不能只让新字段自洽。
            let middle = MarkdownDocumentMetrics::from_text(&text[first..second]);
            let tail = MarkdownDocumentMetrics::from_text(&text[second..]);
            assert_eq!(
                MarkdownDocumentMetrics::joined(
                    MarkdownDocumentMetrics::joined(left, middle),
                    tail
                ),
                full
            );
            assert_eq!(
                MarkdownDocumentMetrics::joined(
                    left,
                    MarkdownDocumentMetrics::joined(middle, tail)
                ),
                full
            );
        }
    }
    // 其它控制符不得混入六种精确存在性；同属S/B不等于相同字符。
    assert_eq!(
        MarkdownDocumentMetrics::from_text("\0\u{b}\u{c}\u{1c}\u{1f}")
            .unicode_properties
            .separators(),
        0
    );
}

#[test]
fn original_spans_edits_undo_and_frozen_roots_keep_independent_exact_sets() {
    let text = "中文😀 ".repeat(32_768);
    let (_directory, mut document) = fixture(&text);
    let backing = document.original_backing_observer().unwrap();
    let original = document.text_snapshot();
    let original_properties = oracle(&text);
    assert_eq!(original.unicode_properties(), Ok(original_properties));
    // 该真实文件跨多个原始span；在目录附近插入新类和所有六控制，必须重算真实新root。
    document
        .insert_utf16(
            65_535,
            "א\t\r\n\u{85}\u{2028}\u{2029}",
            MarkdownTextSelection::caret(65_542),
        )
        .unwrap();
    let inserted = document.text_snapshot();
    let extra = oracle("א\t\r\n\u{85}\u{2028}\u{2029}");
    assert_eq!(
        inserted.unicode_properties(),
        Ok(original_properties.joined(extra))
    );
    assert_eq!(original.unicode_properties(), Ok(original_properties));
    document.undo_text_edit().unwrap();
    assert_eq!(
        document.text_snapshot().unicode_properties(),
        Ok(original_properties)
    );
    document.redo_text_edit().unwrap();
    assert_eq!(
        document.text_snapshot().unicode_properties(),
        inserted.unicode_properties()
    );
    document
        .replace_utf16_range(
            0..document.utf16_len(),
            "中\u{05c8}",
            MarkdownTextSelection::caret(2),
        )
        .unwrap();
    let replacement = document.text_snapshot();
    assert_eq!(replacement.unicode_properties(), Ok(oracle("中\u{05c8}")));
    assert_eq!(
        inserted.unicode_properties(),
        Ok(original_properties.joined(extra))
    );
    drop(document);
    drop(original);
    drop(inserted);
    // 全编辑root不保已删除原始payload；最后引用真实原始根后，映射必须已解除。
    assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
    assert_eq!(replacement.unicode_properties(), Ok(oracle("中\u{05c8}")));
}

#[test]
fn cached_whole_root_queries_do_not_borrow_source_and_preserve_real_eio() {
    for text in ["", "中文😀", "中\t文", "א\r\n"] {
        let (_directory, document) = fixture(text);
        let snapshot = document.text_snapshot();
        let observer = document.original_backing_observer();
        document.reclaim_original_read_windows_for_test().unwrap();
        let before = observer
            .as_ref()
            .map(|receipt| (receipt.managed_resident_bytes(), receipt.reclaim_attempts()));
        for _ in 0..8 {
            assert_eq!(snapshot.unicode_properties(), Ok(oracle(text)));
        }
        assert_eq!(
            observer
                .as_ref()
                .map(|receipt| (receipt.managed_resident_bytes(), receipt.reclaim_attempts())),
            before
        );
        if !text.is_empty() {
            // 实际借用后驱逐失败才建立永久EIO；缓存摘要的正/负事实都必须让出首错。
            snapshot.visit_byte_range(0..1, |_| Ok(())).unwrap();
            document.fail_next_original_reclaim_for_test();
            assert_eq!(
                document.reclaim_original_read_windows_for_test(),
                Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                    libc::EIO
                ))
            );
            assert_eq!(
                snapshot.unicode_properties(),
                Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                    libc::EIO
                ))
            );
        }
    }
}

#[test]
fn exact_properties_have_fixed_bounded_storage_without_enlarging_source_windows() {
    assert_eq!(std::mem::size_of::<MarkdownUnicodeProperties>(), 8);
    assert_eq!(std::mem::size_of::<MarkdownDocumentMetrics>(), 80);
    assert_eq!(std::mem::size_of::<MarkdownPieceTreeNode>(), 112);
    assert_eq!(ORIGINAL_PIECE_BYTES, 4096);
    assert_eq!(EDIT_SEGMENT_BYTES, 65_536);
}

#[test]
fn ascii_scalar_envelope_keeps_real_extrema_across_compact_roots_edits_and_history() {
    let text = format!("中文{}\r\n{}終", "X".repeat(4094), "a".repeat(8192));
    let (_directory, mut document) = fixture(&text);
    let check = |document: &MarkdownDocumentBuffer, expected: &str| {
        assert_eq!(
            document.text_snapshot().unicode_properties().unwrap(),
            oracle(expected)
        );
    };
    check(&document, &text);
    document
        .replace_utf16_range(4098..4098, "fi", MarkdownTextSelection::caret(4100))
        .unwrap();
    let mut expected = text.clone();
    let offset: usize = text.chars().take(4098).map(char::len_utf8).sum();
    expected.insert_str(offset, "fi");
    check(&document, &expected);
    document.undo().unwrap();
    check(&document, &text);
    document.redo().unwrap();
    check(&document, &expected);
    println!(
        "ascii_domain_layout properties_bytes={} metrics_bytes={} node_bytes={}",
        std::mem::size_of::<MarkdownUnicodeProperties>(),
        std::mem::size_of::<MarkdownDocumentMetrics>(),
        std::mem::size_of::<MarkdownPieceTreeNode>()
    );
}
