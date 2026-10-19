//! 用真实FrozenText计量读取并与独立字符域oracle差分；不以替身正文证明零读取。

use std::cell::Cell;

use super::*;

struct CountingSource<'a> {
    text: &'a FrozenText,
    calls: Cell<usize>,
    maximum: Cell<usize>,
}
impl PieceByteSource for CountingSource<'_> {
    fn source_health(&self) -> Result<(), MarkdownDocumentBufferError> {
        self.text.source_health()
    }
    fn piece_bytes(
        &self,
        piece: &MarkdownPiece,
    ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError> {
        // 计量包裹同一个真实匿名映射lease，既不虚构字节，也不绕过真实回收检查。
        let bytes = self.text.piece_bytes(piece)?;
        self.calls.set(self.calls.get() + 1);
        self.maximum.set(self.maximum.get().max(bytes.len()));
        Ok(bytes)
    }

    // 字符域测试包装也保留新局部range的实际lease长度，不扩大到完整叶。
    #[cfg(feature = "paged-shaping-frontier")]
    fn piece_range_bytes(
        &self,
        piece: &MarkdownPiece,
        local: Range<usize>,
    ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError> {
        let bytes = self.text.piece_range_bytes(piece, local)?;
        self.calls.set(self.calls.get() + 1);
        self.maximum.set(self.maximum.get().max(bytes.len()));
        Ok(bytes)
    }
}

fn fixture(source: &str) -> (tempfile::TempDir, MarkdownDocumentBuffer) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("printable-ascii.md");
    std::fs::write(&path, source).unwrap();
    let document = MarkdownDocumentBuffer::open(&path, 4 * 1024 * 1024).unwrap();
    (directory, document)
}

#[test]
fn exact_cached_positive_domain_and_all_subranges_avoid_body_reads() {
    let text = "~Printable ASCII 0123 !".repeat(64 * 1024);
    let (_directory, document) = fixture(&text);
    let snapshot = document.text_snapshot();
    let source = CountingSource {
        text: snapshot.text.as_ref(),
        calls: Cell::new(0),
        maximum: Cell::new(0),
    };
    assert_eq!(
        query(&source, source.text.root.as_deref(), 0..text.len()),
        Ok(true)
    );
    assert_eq!(source.calls.get(), 0);
    assert_eq!(
        query(&source, source.text.root.as_deref(), 1..text.len() - 1),
        Ok(true)
    );
    assert_eq!(source.calls.get(), 0);
    for range in [
        17..273,
        65_535..65_551,
        0..0,
        33..33,
        text.len()..text.len(),
    ] {
        // 原全ASCII节点内所有端点均是scalar边界；非零style和空caret也不应重借边叶。
        assert_eq!(query(&source, source.text.root.as_deref(), range), Ok(true));
    }
    assert_eq!((source.calls.get(), source.maximum.get()), (0, 0));
    // 固定Unicode属性摘要使Metrics/Node各增长8字节；源窗口与原查询读取预算不变。
    assert_eq!(std::mem::size_of::<MarkdownDocumentMetrics>(), 80);
    assert_eq!(std::mem::size_of::<MarkdownPieceTreeNode>(), 112);
}

#[test]
fn partial_mixed_leaves_still_read_the_actual_source_and_validate_both_endpoints() {
    let text = format!("😀{}中", "x".repeat(40 * ORIGINAL_PIECE_BYTES));
    let (_directory, document) = fixture(&text);
    let snapshot = document.text_snapshot();
    let source = CountingSource {
        text: snapshot.text.as_ref(),
        calls: Cell::new(0),
        maximum: Cell::new(0),
    };
    let end = snapshot.utf16_len();
    assert_eq!(
        query(&source, source.text.root.as_deref(), 3..end - 2),
        Ok(true)
    );
    // 两个边叶真正含范围外Unicode，不能借整节点正证书；完整中间ASCII仍不读取。
    assert_eq!(source.calls.get(), 2);
    assert!(source.maximum.get() <= ORIGINAL_PIECE_BYTES);
    assert_eq!(
        query(&source, source.text.root.as_deref(), 1..end - 2),
        Err(MarkdownDocumentBufferError::NotUnicodeBoundary)
    );
    assert_eq!(
        query(&source, source.text.root.as_deref(), 0..end - 2),
        Ok(false)
    ); // 非ASCII实际落入范围时仍为false，不能把邻接正子树推广到整个请求。
}

#[test]
fn controls_unicode_empty_and_surrogate_endpoints_match_a_scalar_oracle() {
    let text = format!(
        "{}\0\u{1f} ~\u{7f}\n\t中😀e\u{301}{}",
        "x".repeat(65535),
        "z".repeat(65536)
    );
    let (_directory, document) = fixture(&text);
    let snapshot = document.text_snapshot();
    let mut points = vec![0];
    let mut units = 0;
    for scalar in text.chars() {
        units += scalar.len_utf16();
        if units % 113 == 0 || (65520..65590).contains(&units) {
            points.push(units);
        }
    }
    points.push(units);
    let all: Vec<_> = text.encode_utf16().collect();
    for (ordinal, start) in points.iter().copied().enumerate() {
        for end in [start, points[(ordinal + 13).min(points.len() - 1)], units] {
            // 两端已经由独立char迭代取得真实标量边界，平面UTF16 oracle明确排除全部C0/DEL。
            let expected = all[start..end]
                .iter()
                .all(|unit| (0x20..=0x7e).contains(unit));
            assert_eq!(snapshot.is_printable_ascii_range(start..end), Ok(expected));
        }
    }
    let emoji = text[..text.find('😀').unwrap()].encode_utf16().count();
    for range in [emoji + 1..emoji + 1, 0..emoji + 1, emoji + 1..units] {
        // 0..低代理项在控制符处已经为false，也必须继续验证末端并返回边界错误。
        assert_eq!(
            snapshot.is_printable_ascii_range(range),
            Err(MarkdownDocumentBufferError::NotUnicodeBoundary)
        );
    }
    assert_eq!(snapshot.is_printable_ascii_range(units..units), Ok(true));
    assert_eq!(
        snapshot.is_printable_ascii_range(units..units + 1),
        Err(MarkdownDocumentBufferError::RangeOutsideDocument)
    );
    assert_eq!(
        snapshot.is_printable_ascii_range(Range { start: 2, end: 1 }),
        Err(MarkdownDocumentBufferError::RangeOutsideDocument)
    );
}

#[test]
fn edits_undo_frozen_identity_and_actual_final_unmap_keep_domain_facts() {
    let (_directory, mut document) = fixture(&"word ".repeat(32_000));
    let observer = document.original_backing_observer().unwrap();
    let printable = document.text_snapshot();
    document
        .insert_utf16(65536, "\t😀", MarkdownTextSelection::caret(65539))
        .unwrap();
    let nonprintable = document.text_snapshot();
    assert_eq!(
        printable.is_printable_ascii_range(0..printable.utf16_len()),
        Ok(true)
    );
    assert_eq!(
        nonprintable.is_printable_ascii_range(0..nonprintable.utf16_len()),
        Ok(false)
    );
    document.undo_text_edit().unwrap();
    assert_eq!(
        document
            .text_snapshot()
            .is_printable_ascii_range(0..document.utf16_len()),
        Ok(true)
    );
    document.redo_text_edit().unwrap();
    assert_eq!(
        document
            .text_snapshot()
            .is_printable_ascii_range(0..document.utf16_len()),
        Ok(false)
    );
    drop(document);
    assert!(!observer.unmap_succeeded());
    assert_eq!(printable.is_printable_ascii_range(65530..65540), Ok(true));
    drop(printable);
    assert!(!observer.unmap_succeeded());
    drop(nonprintable);
    assert!(observer.unmap_succeeded());
    assert_eq!(observer.mapped_bytes(), 0);
}

#[test]
fn real_sticky_source_failure_outranks_cached_false_true_and_invalid_ranges() {
    let (_directory, document) = fixture(&format!("{}\t", "x".repeat(65535)));
    let snapshot = document.text_snapshot();
    let backing = snapshot.text.original_snapshot.as_ref().unwrap();
    snapshot.visit_byte_range(0..1, |_| Ok(())).unwrap();
    backing.fail_next_reclaim_for_test();
    assert!(backing.reclaim_unpinned_for_test().is_err());
    for range in [0..0, 0..65536, 0..65537] {
        // 缓存全假、空值与非法范围均不能在真实Backing失败后重新获得查询权限。
        assert_eq!(
            snapshot.is_printable_ascii_range(range),
            Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                libc::EIO
            ))
        );
    }
}

#[test]
fn actual_failure_between_health_observations_overrides_the_cached_positive_result() {
    struct PoisonAfterFirst<'a> {
        text: &'a FrozenText,
        checks: Cell<usize>,
    }
    impl PieceByteSource for PoisonAfterFirst<'_> {
        fn source_health(&self) -> Result<(), MarkdownDocumentBufferError> {
            let checks = self.checks.get() + 1;
            self.checks.set(checks);
            if checks == 2 {
                // 在两个真实健康观察之间注入原Backing的真实驱逐EIO，模拟另一个共享读取者失败。
                let backing = self.text.original_snapshot.as_ref().unwrap();
                backing.fail_next_reclaim_for_test();
                assert!(backing.reclaim_unpinned_for_test().is_err());
            }
            self.text.source_health()
        }
        fn piece_bytes(
            &self,
            piece: &MarkdownPiece,
        ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError> {
            self.text.piece_bytes(piece)
        }

        // 健康负控仍委托原Frozen字节资源，不因新range入口改变故障来源。
        #[cfg(feature = "paged-shaping-frontier")]
        fn piece_range_bytes(
            &self,
            piece: &MarkdownPiece,
            local: Range<usize>,
        ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError> {
            self.text.piece_range_bytes(piece, local)
        }
    }
    let (_directory, document) = fixture(&"x".repeat(65536));
    let snapshot = document.text_snapshot();
    snapshot.visit_byte_range(0..1, |_| Ok(())).unwrap();
    let source = PoisonAfterFirst {
        text: snapshot.text.as_ref(),
        checks: Cell::new(0),
    };
    assert_eq!(
        query(&source, source.text.root.as_deref(), 0..65536),
        Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
            libc::EIO
        ))
    );
    assert_eq!(source.checks.get(), 2);
}

#[test]
fn whole_root_letter_summary_matches_scalar_domain_and_preserves_node_sizes() {
    for scalar in (0..=255).map(|value| char::from_u32(value).unwrap()) {
        // 独立字符 oracle 覆盖 ASCII 全域及相邻 Unicode；每个来源都是真实文件与持久树。
        let text = scalar.to_string();
        let (_directory, document) = fixture(&text);
        let expected = (' '..='~')
            .contains(&scalar)
            .then(|| scalar.is_ascii_alphabetic());
        assert_eq!(
            document.text_snapshot().printable_ascii_has_letters(),
            Ok(expected)
        );
    }
    let (_directory, document) = fixture("");
    assert_eq!(
        document.text_snapshot().printable_ascii_has_letters(),
        Ok(Some(false))
    );
    // ASCII三态仍只占原一个字节；另附通用Unicode属性摘要的8字节实际布局单独核验。
    assert_eq!(std::mem::size_of::<PrintableAsciiClass>(), 1);
    assert_eq!(std::mem::size_of::<MarkdownDocumentMetrics>(), 80);
    assert_eq!(std::mem::size_of::<MarkdownPieceTreeNode>(), 112);
}

#[test]
fn whole_root_letter_summary_follows_original_spans_edits_and_frozen_undo_roots() {
    let text = "0123 []() ".repeat(32_000);
    let (_directory, mut document) = fixture(&text);
    let backing = document.original_backing_observer().unwrap();
    let neutral = document.text_snapshot();
    assert_eq!(neutral.printable_ascii_has_letters(), Ok(Some(false)));
    // 插入跨目录附近的字母，原 span 拆分与新增叶都必须参与真实根聚合。
    document
        .insert_utf16(65535, "A", MarkdownTextSelection::caret(65536))
        .unwrap();
    let latin = document.text_snapshot();
    assert_eq!(latin.printable_ascii_has_letters(), Ok(Some(true)));
    document
        .insert_utf16(131072, "中", MarkdownTextSelection::caret(131073))
        .unwrap();
    let unicode = document.text_snapshot();
    assert_eq!(unicode.printable_ascii_has_letters(), Ok(None));
    // Undo 恢复旧根的原事实，持有的冻结值始终独立，不能借活动文档的最新代次判定。
    document.undo_text_edit().unwrap();
    assert_eq!(
        document.text_snapshot().printable_ascii_has_letters(),
        Ok(Some(true))
    );
    document.undo_text_edit().unwrap();
    assert_eq!(
        document.text_snapshot().printable_ascii_has_letters(),
        Ok(Some(false))
    );
    assert_eq!(latin.printable_ascii_has_letters(), Ok(Some(true)));
    assert_eq!(unicode.printable_ascii_has_letters(), Ok(None));
    drop(document);
    drop(neutral);
    drop(latin);
    assert!(!backing.unmap_succeeded());
    drop(unicode);
    assert!(backing.unmap_succeeded());
    assert_eq!(backing.mapped_bytes(), 0);
}

#[test]
fn whole_root_letter_summary_keeps_the_original_io_error_above_cached_facts() {
    for text in ["0123 []", "Latin", "中"] {
        let (_directory, document) = fixture(text);
        let snapshot = document.text_snapshot();
        snapshot.visit_byte_range(0..1, |_| Ok(())).unwrap();
        let backing = snapshot.text.original_snapshot.as_ref().unwrap();
        backing.fail_next_reclaim_for_test();
        assert!(backing.reclaim_unpinned_for_test().is_err());
        // 三种缓存结果都不能绕过原 backing 的永久 EIO，且重复查询保留最初错误。
        for _ in 0..2 {
            assert_eq!(
                snapshot.printable_ascii_has_letters(),
                Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                    libc::EIO
                ))
            );
        }
    }
}

#[cfg(feature = "paged-shaping-frontier")]
#[test]
fn complete_item_ascii_envelope_sees_late_tail_with_one_unit_budgets_and_releases() {
    use std::io::Write;
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all("中文\r\nX".as_bytes()).unwrap();
    file.write_all(&[b'a'; 4095]).unwrap();
    for _ in 0..1023 {
        file.write_all(&[b'a'; 4096]).unwrap();
    }
    file.write_all(b"z").unwrap();
    file.flush().unwrap();
    let document = MarkdownDocumentBuffer::open(file.path(), 8 * 1024 * 1024).unwrap();
    let mapping = document.original_backing_observer().unwrap();
    let snapshot = document.text_snapshot();
    let mut cursor = snapshot
        .begin_printable_ascii_suffix(4..snapshot.utf16_len())
        .unwrap();
    let mut bytes = 0;
    let mut ready = None;
    for _ in 0..32_768 {
        let step = snapshot
            .advance_printable_ascii_suffix(&mut cursor, 1, 1, 1)
            .unwrap();
        assert!(
            step.bytes <= 1 && step.scalars <= 1 && step.metadata <= 1 && step.maximum_borrow <= 1
        );
        bytes += step.bytes;
        if let Some(answer) = step.answer {
            ready = Some((answer, step.minimum, step.maximum));
            break;
        }
    }
    assert_eq!(ready, Some((true, b'X', b'z'))); // 真尾z不能被当前首257字节的Xa样本遗漏。
    assert!(
        bytes <= 4096,
        "完整域证明只能读取原混合边叶，不能扫描四MiB尾段"
    );
    let foreign = document.text_snapshot();
    assert_eq!(
        foreign
            .advance_printable_ascii_suffix(&mut cursor, 1, 1, 1)
            .err(),
        Some(MarkdownDocumentBufferError::RangeOutsideDocument)
    );
    drop((foreign, snapshot, document));
    assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0);
}

#[cfg(feature = "paged-shaping-frontier")]
#[test]
fn ascii_envelope_keeps_scalar_endpoint_and_real_source_error_before_old_ready() {
    let (_directory, document) = fixture("中Xa😀fi\r\n");
    let snapshot = document.text_snapshot();
    let mut invalid = snapshot.begin_printable_ascii_suffix(2..4).unwrap();
    let mut error = None;
    for _ in 0..128 {
        match snapshot.advance_printable_ascii_suffix(&mut invalid, 1, 1, 1) {
            Ok(step) => assert!(step.answer.is_none()),
            Err(cause) => {
                error = Some(cause);
                break;
            }
        }
    }
    assert_eq!(error, Some(MarkdownDocumentBufferError::NotUnicodeBoundary));
    let mut ready = snapshot.begin_printable_ascii_suffix(1..3).unwrap();
    for _ in 0..128 {
        if snapshot
            .advance_printable_ascii_suffix(&mut ready, 1, 1, 1)
            .unwrap()
            .answer
            .is_some()
        {
            break;
        }
    }
    snapshot.visit_byte_range(0..1, |_| Ok(())).unwrap();
    document.fail_next_original_reclaim_for_test();
    assert!(document.reclaim_original_read_windows_for_test().is_err());
    assert_eq!(
        snapshot
            .advance_printable_ascii_suffix(&mut ready, 1, 1, 1)
            .err(),
        Some(MarkdownDocumentBufferError::OriginalBackingUnavailable(
            libc::EIO
        ))
    );
}
