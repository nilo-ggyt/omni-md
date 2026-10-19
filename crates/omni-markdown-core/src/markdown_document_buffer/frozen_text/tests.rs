//! 仅用独占合成文件验证冻结值的Unicode、COW、事务和真实资源所有权，不接触用户笔记。

use std::io::Write;
use std::rc::Rc;

use tempfile::{TempDir, tempdir};

use super::*;

fn fixture(source: &str) -> (TempDir, MarkdownDocumentBuffer) {
    // 文件只用于建立真实独立匿名映射；快照不依赖此目录或原路径继续存在。
    let directory = tempdir().unwrap();
    let path = directory.path().join("frozen-text.md");
    std::fs::write(&path, source).unwrap();
    let document = MarkdownDocumentBuffer::open(&path, 8 * 1024 * 1024).unwrap();
    (directory, document)
}

fn assert_text(snapshot: &MarkdownTextSnapshot, expected: &str) {
    // 完整oracle仅属于有限合成测试；生产快照没有wholeString或自有UTF-16数组。
    let expected_units: Vec<u16> = expected.encode_utf16().collect();
    let mut actual = vec![0; expected_units.len()];
    snapshot.copy_utf16_code_units(0, &mut actual).unwrap();
    assert_eq!(snapshot.byte_len(), expected.len());
    assert_eq!(snapshot.utf16_len(), expected_units.len());
    assert_eq!(actual, expected_units);
    let mut bytes = Vec::new();
    snapshot
        .visit_byte_range(0..snapshot.byte_len(), |piece| {
            // 原始叶与编辑叶都最多4KiB；大请求仍只按借用逐叶交付。
            assert!(piece.len() <= ORIGINAL_PIECE_BYTES);
            bytes.extend_from_slice(piece);
            Ok(())
        })
        .unwrap();
    assert_eq!(bytes, expected.as_bytes());
}

#[test]
fn empty_snapshot_has_only_empty_boundaries_and_no_retained_payload() {
    let (_directory, document) = fixture("");
    let snapshot = document.text_snapshot();
    assert_text(&snapshot, "");
    assert!(snapshot.text.root.is_none());
    assert!(snapshot.text.original_snapshot.is_none());
    assert!(snapshot.text.edits.is_empty());
    assert_eq!(snapshot.byte_offset_for_utf16(0), Ok(0));
    assert_eq!(snapshot.utf16_offset_for_byte(0), Ok(0));
    assert_eq!(snapshot.copy_utf16_code_units(0, &mut []), Ok(()));
    assert_eq!(
        snapshot.utf16_code_unit_at(0),
        Err(MarkdownDocumentBufferError::RangeOutsideDocument)
    );
    assert_eq!(
        snapshot.byte_offset_for_utf16(1),
        Err(MarkdownDocumentBufferError::RangeOutsideDocument)
    );
    assert_eq!(
        snapshot.utf16_offset_for_byte(1),
        Err(MarkdownDocumentBufferError::RangeOutsideDocument)
    );
}

#[test]
fn scalar_coordinates_and_surrogate_unit_reads_keep_distinct_contracts() {
    let (_directory, document) = fixture("a中😀e\u{301}z");
    let snapshot = document.text_snapshot();
    for (byte, utf16) in [(0, 0), (1, 1), (4, 2), (8, 4), (9, 5), (11, 6), (12, 7)] {
        // 所有真实标量边界双向往返；编码内部位置仅允许以原始单元方式读取。
        assert_eq!(snapshot.byte_offset_for_utf16(utf16), Ok(byte));
        assert_eq!(snapshot.utf16_offset_for_byte(byte), Ok(utf16));
    }
    assert_eq!(snapshot.utf16_code_unit_at(2), Ok(0xd83d));
    assert_eq!(snapshot.utf16_code_unit_at(3), Ok(0xde00));
    let mut low_and_next = [0; 2];
    snapshot
        .copy_utf16_code_units(3, &mut low_and_next)
        .unwrap();
    assert_eq!(low_and_next, [0xde00, u16::from(b'e')]);
    assert_eq!(
        snapshot.byte_offset_for_utf16(3),
        Err(MarkdownDocumentBufferError::NotUnicodeBoundary)
    );
    assert_eq!(
        snapshot.utf16_offset_for_byte(6),
        Err(MarkdownDocumentBufferError::NotUnicodeBoundary)
    );

    // 溢出、越界与EOF读取在第一次写入前失败，不静默截断调用方的请求。
    let mut unchanged = [0xbeef; 2];
    assert_eq!(
        snapshot.copy_utf16_code_units(usize::MAX, &mut unchanged),
        Err(MarkdownDocumentBufferError::RangeOutsideDocument)
    );
    assert_eq!(
        snapshot.copy_utf16_code_units(snapshot.utf16_len() - 1, &mut unchanged),
        Err(MarkdownDocumentBufferError::RangeOutsideDocument)
    );
    assert_eq!(unchanged, [0xbeef; 2]);
    assert_eq!(
        snapshot.utf16_code_unit_at(snapshot.utf16_len()),
        Err(MarkdownDocumentBufferError::RangeOutsideDocument)
    );
}

#[test]
fn zero_utf16_boundary_keeps_far_original_pages_and_reports_sticky_reclaim_failure() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("zero-boundary.md");
    let mut file = std::fs::File::create(&path).unwrap();
    let block = [b'a'; 4096];
    for _ in 0..64 {
        file.write_all(&block).unwrap(); // 真实256KiB源只按固定块生成，不物化整文。
    }
    drop(file);
    let mut document = MarkdownDocumentBuffer::open(&path, 1024 * 1024).unwrap();
    let backing = document.original_backing_observer().unwrap();
    let original = document.text_snapshot();
    document
        .replace_utf16_range(0..0, "😀", MarkdownTextSelection::caret(2))
        .unwrap();
    let edited = document.text_snapshot();
    assert!(document.undo().is_some());
    let undone = document.text_snapshot();
    assert!(document.redo().is_some());
    assert_eq!(
        edited.byte_offset_for_utf16(1),
        Err(MarkdownDocumentBufferError::NotUnicodeBoundary)
    );
    assert_eq!(edited.byte_offset_for_utf16(2), Ok(4));
    assert_eq!(edited.byte_offset_for_utf16(3), Ok(5)); // 非零代理项和原片偏移仍沿原解码路径。

    // 填满真实远端64KiB页目录；旧0查询会请求首系统页并驱逐最旧远端页。
    // 使用4KiB步长同时覆盖4KiB与本机16KiB页，不依赖目录命中次数的模拟值。
    let far = 128 * 1024;
    for offset in (far..far + 64 * 1024).step_by(4096) {
        original
            .visit_byte_range(offset..offset + 1, |bytes| {
                assert_eq!(bytes, b"a");
                Ok(())
            })
            .unwrap();
    }
    assert_eq!(backing.managed_resident_bytes(), 64 * 1024);
    let before = (backing.reclaim_attempts(), backing.reclaim_successes());
    for snapshot in [&original, &edited, &undone] {
        assert_eq!(snapshot.byte_offset_for_utf16(0), Ok(0));
    }
    assert_eq!(document.byte_offset_for_utf16(0), Ok(0));
    assert_eq!(
        (backing.reclaim_attempts(), backing.reclaim_successes()),
        before
    );
    original
        .visit_byte_range(far..far + 1, |bytes| {
            assert_eq!(bytes, b"a");
            Ok(())
        })
        .unwrap();
    assert_eq!(
        (backing.reclaim_attempts(), backing.reclaim_successes()),
        before
    );

    // 原远端页确实存在，只有真实msync成功后的一次注入才令全共享源永久失败。
    document.fail_next_original_reclaim_for_test();
    let failure = MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO);
    assert_eq!(
        document.reclaim_original_read_windows_for_test(),
        Err(failure)
    );
    assert!(backing.reclaim_attempts() > before.0 && backing.reclaim_successes() > before.1);
    assert_eq!(backing.first_errno(), Some(libc::EIO));
    for snapshot in [&original, &edited, &undone] {
        assert_eq!(snapshot.byte_offset_for_utf16(0), Err(failure));
    }
    assert_eq!(document.byte_offset_for_utf16(0), Err(failure));
    drop((document, original, edited, undone));
    assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0);
    assert!(backing.failed()); // 实际unmap成功不能擦掉先前的真实源回收错误记录。
}

#[test]
fn capture_and_clone_share_exact_tree_mapping_and_edit_allocations() {
    let source = "中文😀原文\r\n".repeat(32_000);
    let (_directory, mut document) = fixture(&source);
    document
        .replace_utf16_range(2..2, "有限编辑", MarkdownTextSelection::caret(2))
        .unwrap();
    let before = document.storage_profile();
    let snapshot = document.text_snapshot();
    let cloned = snapshot.clone();

    // 以真实资源身份验证共享；内存布局没有第二份正文，不能只比较字符结果推断零复制。
    assert!(Arc::ptr_eq(&snapshot.text, &cloned.text));
    assert!(Arc::ptr_eq(
        document.root.as_ref().unwrap(),
        snapshot.text.root.as_ref().unwrap()
    ));
    assert!(Arc::ptr_eq(
        document.original_snapshot.as_ref().unwrap(),
        snapshot.text.original_snapshot.as_ref().unwrap()
    ));
    assert_eq!(snapshot.text.edits.len(), 1);
    assert!(Arc::ptr_eq(
        &document.edits.segments[0],
        &snapshot.text.edits[&0]
    ));
    assert_eq!(document.storage_profile(), before);
    assert_eq!(snapshot.text.edits[&0].as_slice(), "有限编辑".as_bytes());
}

#[test]
fn appending_after_freeze_copies_only_shared_tail_and_keeps_segment_capacity() {
    let (_directory, mut document) = fixture("原");
    let first_edit = "x".repeat(EDIT_SEGMENT_BYTES + 20);
    document
        .replace_utf16_range(
            1..1,
            &first_edit[..EDIT_SEGMENT_BYTES],
            MarkdownTextSelection::caret(1),
        )
        .unwrap();
    // 两次真实小输入继续覆盖原heap尾段COW；一次超段长输入现在属于独立封存映射职责。
    document
        .replace_utf16_range(
            1 + EDIT_SEGMENT_BYTES..1 + EDIT_SEGMENT_BYTES,
            &first_edit[EDIT_SEGMENT_BYTES..],
            MarkdownTextSelection::caret(1),
        )
        .unwrap();
    let frozen = document.text_snapshot();
    let first_segment = Arc::clone(&document.edits.segments[0]);
    let original_tail = Arc::clone(&document.edits.segments[1]);
    let end = document.utf16_len();
    document
        .replace_utf16_range(end..end, "后😀", MarkdownTextSelection::caret(end))
        .unwrap();

    // 完整前段始终共享；仅实际写入的尾段换allocation，原编号与累计字节预算保持原值。
    assert!(Arc::ptr_eq(&first_segment, &document.edits.segments[0]));
    assert!(!Arc::ptr_eq(&original_tail, &document.edits.segments[1]));
    assert!(Arc::ptr_eq(&original_tail, &frozen.text.edits[&1]));
    assert_eq!(original_tail.as_slice(), &[b'x'; 20]);
    assert_eq!(document.edits.segments[1].capacity(), EDIT_SEGMENT_BYTES);
    assert_eq!(document.edits.used_bytes, first_edit.len() + "后😀".len());
    assert_text(&frozen, &format!("原{first_edit}"));
    assert_text(&document.text_snapshot(), &format!("原{first_edit}后😀"));
}

#[test]
fn old_values_survive_cross_piece_edits_undo_redo_and_document_drop() {
    let source = format!(
        "{}中😀尾\r\n{}",
        "a".repeat(ORIGINAL_PIECE_BYTES - 3),
        "z".repeat(32)
    );
    let (directory, mut document) = fixture(&source);
    let original = document.text_snapshot();
    let start = ORIGINAL_PIECE_BYTES - 4;
    let end = ORIGINAL_PIECE_BYTES + 4;
    let start_byte = document.byte_offset_for_utf16(start).unwrap();
    let end_byte = document.byte_offset_for_utf16(end).unwrap();
    let changed = format!("{}替😀{}", &source[..start_byte], &source[end_byte..]);
    document
        .replace_utf16_range(start..end, "替😀", MarkdownTextSelection::caret(start))
        .unwrap();
    let edited = document.text_snapshot();
    document.undo_text_edit().unwrap();
    assert_text(&document.text_snapshot(), &source);
    document.redo_text_edit().unwrap();
    assert_text(&document.text_snapshot(), &changed);
    document
        .replace_utf16_range(start..start + 3, "新", MarkdownTextSelection::caret(start))
        .unwrap();

    // 保存的两个值不随当前generation、历史root或用户路径存活；目录也可以先释放。
    drop(document);
    drop(directory);
    assert_text(&original, &source);
    assert_text(&edited, &changed);
}

#[test]
fn snapshot_outlives_live_document_without_retaining_document_or_path() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<MarkdownTextSnapshot>();
    let (directory, document) = fixture("真实独立😀正文");
    let original_mapping = Arc::downgrade(document.original_snapshot.as_ref().unwrap());
    let original_root = Arc::downgrade(document.root.as_ref().unwrap());
    let document = Rc::new(RefCell::new(document));
    let weak_document = Rc::downgrade(&document);
    let snapshot = document.borrow().text_snapshot();
    let cloned = snapshot.clone();
    std::fs::write(document.borrow().path(), "外部截短").unwrap();
    drop(document);
    drop(directory);

    // 原文档已经真实销毁；匿名mmap与原root由值而非路径或文档保活。
    assert!(weak_document.upgrade().is_none());
    assert!(original_mapping.upgrade().is_some());
    assert!(original_root.upgrade().is_some());
    assert_text(&snapshot, "真实独立😀正文");
    drop(snapshot);
    assert_text(&cloned, "真实独立😀正文");
    drop(cloned);
    assert!(original_mapping.upgrade().is_none());
    assert!(original_root.upgrade().is_none());
}

#[test]
fn recovery_snapshot_keeps_original_payload_offset_without_record_bytes() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("synthetic-record");
    let prefix = b"record-header:";
    let payload = "恢复😀正文";
    let record = [prefix.as_slice(), payload.as_bytes(), b":record-tail"].concat();
    std::fs::write(&path, &record).unwrap();
    let file = std::fs::File::open(&path).unwrap();
    let mapping = snapshot_canonical(&file, record.len() as u64, || true)
        .unwrap()
        .unwrap();
    let document = MarkdownDocumentBuffer::from_legacy_recovery(
        &path,
        mapping,
        prefix.len()..prefix.len() + payload.len(),
        [0; 32],
        1024,
    )
    .unwrap();
    let snapshot = document.text_snapshot();
    drop(document);
    drop(file);
    drop(directory);

    // 旧record的header/path/tail不是正文坐标；所有查询仍只见真实payload0..EOF。
    assert_text(&snapshot, payload);
    assert_eq!(snapshot.byte_offset_for_utf16(2), Ok("恢复".len()));
    assert_eq!(snapshot.utf16_code_unit_at(3), Ok(0xde00));
}

#[test]
fn snapshot_retains_only_sources_in_current_root_and_releases_last_edit_arc() {
    let (_directory, mut document) = fixture("原文");
    let original_mapping = Arc::downgrade(document.original_snapshot.as_ref().unwrap());
    document
        .replace_utf16_range(0..2, "只有编辑😀", MarkdownTextSelection::caret(0))
        .unwrap();
    let edited = document.text_snapshot();
    let edit_segment = Arc::downgrade(&document.edits.segments[0]);
    assert!(edited.text.original_snapshot.is_none());
    drop(document);

    // 完全被替换的原始映射仅供旧history使用，不应跟随当前值存活。
    assert!(original_mapping.upgrade().is_none());
    assert!(edit_segment.upgrade().is_some());
    assert_text(&edited, "只有编辑😀");
    drop(edited);
    assert!(edit_segment.upgrade().is_none());

    let (_directory, mut document) = fixture("保留原文");
    let large_edit = "x".repeat(EDIT_SEGMENT_BYTES * 2 + 1);
    document
        .replace_utf16_range(0..0, &large_edit, MarkdownTextSelection::caret(0))
        .unwrap();
    let abandoned: Vec<_> = document.edits.segments.iter().map(Arc::downgrade).collect();
    document
        .replace_utf16_range(0..large_edit.len(), "", MarkdownTextSelection::caret(0))
        .unwrap();
    let current = document.text_snapshot();
    assert!(current.text.edits.is_empty());
    drop(document);
    for segment in abandoned {
        // 被删掉的历史段逐一实际释放，不能仅通过快照的空字段声称已释放。
        assert!(segment.upgrade().is_none());
    }
    assert_text(&current, "保留原文");
}

#[test]
fn prepared_input_freeze_and_cancellation_preserve_all_published_values() {
    let (_directory, mut document) = fixture("原");
    document
        .replace_utf16_range(1..1, "已提交", MarkdownTextSelection::caret(1))
        .unwrap();
    let before = document.text_snapshot();
    let before_profile = document.storage_profile();
    let before_root = document.root.clone();
    let end = document.utf16_len();
    let (token, ()) = document
        .prepare_text_input_observed(
            end..end,
            "未发布😀候选",
            document.selection(),
            MarkdownTextSelection::caret(end),
            changes::NoopObserver,
        )
        .unwrap();
    let during = document.text_snapshot();
    let shared_candidate_tail = Arc::clone(document.edits.segments.last().unwrap());
    assert_text(&during, "原已提交");
    document.cancel_prepared_text_input(&token).unwrap();

    // 取消时新快照可能共享包含未发布后缀的段；只COW恢复活动段，不改任何冻结值。
    assert!(!Arc::ptr_eq(
        &shared_candidate_tail,
        document.edits.segments.last().unwrap()
    ));
    assert_eq!(document.storage_profile(), before_profile);
    assert!(piece_tree_links_share_root(&document.root, &before_root));
    assert!(!document.has_prepared_text_input());
    assert_text(&before, "原已提交");
    assert_text(&during, "原已提交");
    assert_text(&document.text_snapshot(), "原已提交");
}

#[test]
/// 长输入沿原编号封存，后继小输入、历史和独立snapshot都不把文件重新物化为堆字节。
fn long_edit_keeps_frozen_segment_through_small_input_history_and_final_snapshot_drop() {
    let (_directory, mut document) = fixture("原");
    let input = "中😀a\r\n".repeat(32_000);
    document
        .replace_utf16_range(1..1, &input, MarkdownTextSelection::caret(1))
        .unwrap();
    let MarkdownEditSegment::Frozen(backing) = document.edits.segments[0].as_ref() else {
        panic!("长输入应由真实匿名映射保管");
    };
    let observer = backing.observer();
    assert_eq!(observer.mapped_bytes(), input.len());
    assert_eq!(document.edits.allocated_bytes(), 0);
    let frozen = document.text_snapshot();
    let end = document.utf16_len();
    document
        .replace_utf16_range(end..end, "尾", MarkdownTextSelection::caret(end + 1))
        .unwrap();
    assert!(matches!(
        document.edits.segments[1].as_ref(),
        MarkdownEditSegment::Heap(_)
    ));
    assert!(Arc::ptr_eq(
        &frozen.text.edits[&0],
        &document.edits.segments[0]
    ));
    assert_text(&frozen, &format!("原{input}"));
    assert!(document.undo().is_some());
    assert_text(&document.text_snapshot(), &format!("原{input}"));
    assert!(document.undo().is_some());
    assert_text(&document.text_snapshot(), "原");
    assert!(document.redo().is_some());
    assert_text(&document.text_snapshot(), &format!("原{input}"));
    assert!(observer.peak_managed_resident_bytes() <= 64 * 1024);
    drop(document);
    assert_eq!(observer.mapped_bytes(), input.len()); // 最后实际snapshot仍拥有该段，不能提前签释放。
    assert_text(&frozen, &format!("原{input}"));
    drop(frozen);
    assert_eq!(observer.mapped_bytes(), 0);
    assert!(observer.unmap_succeeded());
}

#[test]
/// 未发布封存段在原cancel中先随候选root退场，再回退同一个段表，旧正文与历史均保留。
fn cancelling_prepared_long_edit_unmaps_only_the_unpublished_segment() {
    let (_directory, mut document) = fixture("原文");
    let before = document.storage_profile();
    let root = document.root.clone();
    let (token, ()) = document
        .prepare_text_input_observed(
            1..1,
            &"中😀".repeat(32_000),
            document.selection(),
            MarkdownTextSelection::caret(1),
            changes::NoopObserver,
        )
        .unwrap();
    let MarkdownEditSegment::Frozen(backing) = document.edits.segments.last().unwrap().as_ref()
    else {
        panic!("长候选应封存在匿名映射");
    };
    let observer = backing.observer();
    assert!(observer.mapped_bytes() > EDIT_SEGMENT_BYTES);
    assert_text(&document.text_snapshot(), "原文");
    document.cancel_prepared_text_input(&token).unwrap();
    assert_eq!(document.storage_profile(), before);
    assert!(piece_tree_links_share_root(&document.root, &root));
    assert_eq!(observer.mapped_bytes(), 0);
    assert!(observer.unmap_succeeded());
    assert_text(&document.text_snapshot(), "原文");
}

#[test]
/// 封存编辑段的真实页回收错误永久撤销缓存读取和后继准备，不能被原始映射健康遮住。
fn frozen_edit_reclaim_failure_reaches_metadata_snapshots_and_next_input() {
    let (_directory, mut document) = fixture("原");
    document
        .replace_utf16_range(
            1..1,
            &"x".repeat(256 * 1024),
            MarkdownTextSelection::caret(1),
        )
        .unwrap();
    let frozen = document.text_snapshot();
    let MarkdownEditSegment::Frozen(backing) = document.edits.segments[0].as_ref() else {
        panic!("应取得原封存编辑文件");
    };
    let observer = backing.observer();
    backing.fail_next_reclaim_for_test();
    assert_eq!(
        backing.reclaim_unpinned_for_test(),
        Err(crate::original_backing::OriginalBackingError::Unavailable(
            libc::EIO
        ))
    );
    let error = MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO);
    assert_eq!(frozen.byte_offset_for_utf16(0), Err(error));
    assert_eq!(frozen.is_printable_ascii_range(0..0), Err(error));
    assert_eq!(document.byte_offset_for_utf16(0), Err(error));
    assert!(
        matches!(document.prepare_text_input_observed(0..0, "后", document.selection(), MarkdownTextSelection::caret(0), changes::NoopObserver), Err(changes::MarkdownObservedEditError::Document(found)) if found == error)
    );
    assert!(observer.failed());
    drop(document);
    assert!(observer.mapped_bytes() > 0);
    drop(frozen);
    assert!(observer.unmap_succeeded());
}

#[test]
fn bounded_visitor_borrows_actual_piece_bytes_and_stops_on_first_failure() {
    let source = format!(
        "{}😀中{}",
        "x".repeat(ORIGINAL_PIECE_BYTES - 1),
        "z".repeat(32)
    );
    let (_directory, document) = fixture(&source);
    let snapshot = document.text_snapshot();
    let start = ORIGINAL_PIECE_BYTES - 2;
    let end = ORIGINAL_PIECE_BYTES + 5;
    let mapping = snapshot.text.original_snapshot.as_ref().unwrap();
    let mut offset = start;
    snapshot
        .visit_byte_range(start..end, |piece| {
            // 指针来自原mmap的真实偏移；即使范围位于UTF-8标量内部也不复制或改字符。
            mapping
                .with_bytes(offset..offset + piece.len(), |same| {
                    assert_eq!(piece.as_ptr(), same.as_ptr());
                })
                .unwrap();
            assert_eq!(piece, &source.as_bytes()[offset..offset + piece.len()]);
            offset += piece.len();
            Ok(())
        })
        .unwrap();
    assert_eq!(offset, end);

    let mut calls = 0;
    assert_eq!(
        snapshot.visit_byte_range(0..source.len(), |_| {
            calls += 1;
            Err(MarkdownDocumentBufferError::FindInterrupted)
        }),
        Err(MarkdownDocumentBufferError::FindInterrupted)
    );
    assert_eq!(calls, 1);
    assert_eq!(
        snapshot.visit_byte_range(Range { start: 3, end: 2 }, |_| {
            calls += 1;
            Ok(())
        }),
        Err(MarkdownDocumentBufferError::RangeOutsideDocument)
    );
    assert_eq!(calls, 1);
}

#[test]
fn frozen_cache_lock_poison_is_not_recovered_as_a_valid_immutable_read() {
    let (_directory, document) = fixture("冻结原值😀");
    let snapshot = document.text_snapshot();
    assert_eq!(snapshot.utf16_code_unit_at(0), Ok(0x51bb));
    let worker = snapshot.clone();
    // 只有合成测试主动在持锁时panic；后台异常不得将半写入窗口重新声明为有效。
    assert!(
        std::thread::spawn(move || {
            let _guard = worker.text.utf16_read_cache.lock().unwrap();
            panic!("合成冻结cache锁poison");
        })
        .join()
        .is_err()
    );
    assert_eq!(
        snapshot.utf16_code_unit_at(0),
        Err(MarkdownDocumentBufferError::Utf16ReadCacheUnavailable)
    );
    let mut output = [0xbeef];
    assert_eq!(
        snapshot.copy_utf16_code_units(0, &mut output),
        Err(MarkdownDocumentBufferError::Utf16ReadCacheUnavailable)
    );
    assert_eq!(output, [0xbeef]);
    // 同一Doc和后续独立capture也共享原poison，不能重建一个干净cache绕过失败。
    assert_eq!(
        document.utf16_code_unit_at(0),
        Err(MarkdownDocumentBufferError::Utf16ReadCacheUnavailable)
    );
    assert_eq!(
        document.text_snapshot().utf16_code_unit_at(0),
        Err(MarkdownDocumentBufferError::Utf16ReadCacheUnavailable)
    );
}

#[test]
fn hundreds_of_distinct_captures_share_the_document_single_actual_8k_window() {
    let (directory, document) = fixture(&"原😀文".repeat(4096));
    let mapping = document.original_backing_observer().unwrap();
    let cache = Arc::downgrade(&document.utf16_read_cache);
    assert!(
        document
            .utf16_read_cache
            .lock()
            .unwrap()
            .allocated_window_for_test()
            .is_none()
    );
    document.utf16_code_unit_at(0).unwrap();
    let original_window = document
        .utf16_read_cache
        .lock()
        .unwrap()
        .allocated_window_for_test()
        .unwrap();
    assert_eq!(original_window.1, 8192);
    let mut snapshots = Vec::new();
    for capture in 0..512 {
        // 每次调用都产生独立FrozenText payload，不是用snapshot.clone掩盖capture重复分配。
        let snapshot = document.text_snapshot();
        assert!(Arc::ptr_eq(
            &document.utf16_read_cache,
            &snapshot.text.utf16_read_cache
        ));
        assert_eq!(snapshot.text.generation, document.generation);
        let offset = (capture * 97) % document.utf16_len();
        assert_eq!(
            snapshot.utf16_code_unit_at(offset),
            document.utf16_code_unit_at(offset)
        );
        assert_eq!(
            snapshot
                .text
                .utf16_read_cache
                .lock()
                .unwrap()
                .allocated_window_for_test(),
            Some(original_window)
        );
        snapshots.push(snapshot);
    }
    assert_eq!(Arc::strong_count(&document.utf16_read_cache), 513);
    drop(document);
    drop(directory);
    assert!(!mapping.unmap_succeeded());
    assert!(cache.upgrade().is_some());
    for snapshot in &snapshots {
        assert_eq!(snapshot.utf16_code_unit_at(0), Ok(0x539f));
    }
    drop(snapshots);
    assert!(cache.upgrade().is_none());
    assert!(mapping.unmap_succeeded());
    assert_eq!(mapping.mapped_bytes(), 0);
    assert!(!mapping.failed());
}

#[test]
fn interleaved_old_and_new_generations_reuse_one_box_without_changing_frozen_values() {
    let (_directory, mut document) = fixture("旧😀文");
    let original = document.text_snapshot();
    original.utf16_code_unit_at(0).unwrap();
    let original_window = original
        .text
        .utf16_read_cache
        .lock()
        .unwrap()
        .allocated_window_for_test()
        .unwrap();
    document
        .replace_utf16_range(0..1, "新", MarkdownTextSelection::caret(1))
        .unwrap();
    let changed = document.text_snapshot();
    assert_ne!(original.text.generation, changed.text.generation);
    document.undo_text_edit().unwrap();
    let restored = document.text_snapshot();
    assert_ne!(original.text.generation, restored.text.generation);
    // Undo可恢复相同root；原捕获代次仍不同，不能用活动代次替代冻结键。
    assert!(Arc::ptr_eq(
        original.text.root.as_ref().unwrap(),
        restored.text.root.as_ref().unwrap()
    ));
    for _ in 0..128 {
        assert_eq!(original.utf16_code_unit_at(0), Ok(0x65e7));
        assert_eq!(changed.utf16_code_unit_at(0), Ok(0x65b0));
        assert_eq!(restored.utf16_code_unit_at(0), Ok(0x65e7));
        assert_eq!(document.utf16_code_unit_at(0), Ok(0x65e7));
        assert_eq!(
            document
                .utf16_read_cache
                .lock()
                .unwrap()
                .allocated_window_for_test(),
            Some(original_window)
        );
    }
    document.redo_text_edit().unwrap();
    assert_eq!(document.utf16_code_unit_at(0), Ok(0x65b0));
    drop(document);
    assert_text(&original, "旧😀文");
    assert_text(&changed, "新😀文");
    assert_text(&restored, "旧😀文");
}
