//! Isolated synthetic files verify Unicode, copy-on-write, transactions, and
//! actual frozen-resource ownership without accessing user notes.

use std::io::Write;
use std::rc::Rc;

use tempfile::{TempDir, tempdir};

use super::*;

/// Open the production buffer over a real file owned by the returned temporary directory.
fn fixture(source: &str) -> (TempDir, MarkdownDocumentBuffer) {
    // The file establishes an independent anonymous backing. A snapshot need
    // not retain this directory or require the original path to remain present.
    let directory = tempdir().unwrap();
    let path = directory.path().join("frozen-text.md");
    std::fs::write(&path, source).unwrap();
    let document = MarkdownDocumentBuffer::open(&path, 8 * 1024 * 1024).unwrap();
    (directory, document)
}

/// Compare finite oracle bytes and UTF-16 units through the real snapshot read APIs.
fn assert_text(snapshot: &MarkdownTextSnapshot, expected: &str) {
    // Contiguous output belongs only to this finite synthetic oracle. Production
    // snapshots provide neither a whole-string accessor nor a private UTF-16 body array.
    let expected_units: Vec<u16> = expected.encode_utf16().collect();
    let mut actual = vec![0; expected_units.len()];
    snapshot.copy_utf16_code_units(0, &mut actual).unwrap();
    assert_eq!(snapshot.byte_len(), expected.len());
    assert_eq!(snapshot.utf16_len(), expected_units.len());
    assert_eq!(actual, expected_units);
    let mut bytes = Vec::new();
    snapshot
        .visit_byte_range(0..snapshot.byte_len(), |piece| {
            // Original and edited logical leaves are at most 4 KiB. A larger
            // explicit request still delivers individual borrowed pieces.
            assert!(piece.len() <= ORIGINAL_PIECE_BYTES);
            bytes.extend_from_slice(piece);
            Ok(())
        })
        .unwrap();
    assert_eq!(bytes, expected.as_bytes());
}

#[test]
fn empty_snapshot_has_only_empty_boundaries_and_no_retained_payload() {
    // Actual empty text permits zero boundaries but no readable unit or source
    // resource. It must not behave as a missing backing for nonempty content.
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
    // Scalar-coordinate conversion and native UTF-16 unit observation are
    // separate contracts, especially inside this actual supplementary scalar.
    let (_directory, document) = fixture("a中😀e\u{301}z");
    let snapshot = document.text_snapshot();
    for (byte, utf16) in [(0, 0), (1, 1), (4, 2), (8, 4), (9, 5), (11, 6), (12, 7)] {
        // Round-trip every actual scalar boundary; encoding-interior positions
        // remain usable only through the raw-unit observation contract.
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

    // Overflow, outside extents, and EOF reads fail before mutating output,
    // rather than silently truncating the caller's requested range.
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
    // Zero-coordinate metadata queries must not acquire the first original page
    // and evict a genuinely resident far-source window.
    let directory = tempdir().unwrap();
    let path = directory.path().join("zero-boundary.md");
    let mut file = std::fs::File::create(&path).unwrap();
    let block = [b'a'; 4096];
    for _ in 0..64 {
        // Generate the real 256 KiB source through fixed blocks, without a whole-body fixture copy.
        file.write_all(&block).unwrap();
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
    // Nonzero surrogate and original-piece offsets still use the real scalar decoder.
    assert_eq!(edited.byte_offset_for_utf16(3), Ok(5));

    // Fill the actual far 64 KiB managed-page set; the old zero-boundary read
    // would acquire the first system page and evict its oldest far page. A 4 KiB
    // stride covers both 4 KiB and this machine's 16 KiB pages without simulated hits.
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

    // This existing test injects failure after a real successful msync over
    // actual resident pages; it does not establish an operating-system msync failure.
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
    // Successful actual unmap cannot erase the earlier injected source-failure fact.
    assert!(backing.failed());
}

#[test]
fn capture_and_clone_share_exact_tree_mapping_and_edit_allocations() {
    // Capture a mixed original/edited value, then compare real allocation
    // identities rather than inferring zero copying from equal characters alone.
    let source = "中文😀原文\r\n".repeat(32_000);
    let (_directory, mut document) = fixture(&source);
    document
        .replace_utf16_range(2..2, "有限编辑", MarkdownTextSelection::caret(2))
        .unwrap();
    let before = document.storage_profile();
    let snapshot = document.text_snapshot();
    let cloned = snapshot.clone();

    // Validate sharing through the tree, backing, and edit-segment identities;
    // snapshot acquisition must not append bytes or allocate another body.
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
    // Keep the existing heap-tail COW path active using separate finite inputs,
    // while a snapshot continues owning the original segment allocations.
    let (_directory, mut document) = fixture("原");
    let first_edit = "x".repeat(EDIT_SEGMENT_BYTES + 20);
    document
        .replace_utf16_range(
            1..1,
            &first_edit[..EDIT_SEGMENT_BYTES],
            MarkdownTextSelection::caret(1),
        )
        .unwrap();
    // Two actual small inputs exercise heap-tail COW. One input exceeding the
    // segment size now belongs to the independently frozen-file path instead.
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

    // Completed earlier segments remain shared. Only the actually modified
    // tail changes allocation, preserving stable IDs and cumulative byte accounting.
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
    // Put mixed-width text across an original piece boundary and retain values
    // on both sides of a real edit before exercising undo/redo and later input.
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

    // The two retained values depend on neither current generation/history nor
    // a live path. Release the active document and fixture directory first.
    drop(document);
    drop(directory);
    assert_text(&original, &source);
    assert_text(&edited, &changed);
}

#[test]
fn snapshot_outlives_live_document_without_retaining_document_or_path() {
    // The immutable value is cross-thread capable, while the actual active
    // document remains a separately observed owner that must be destroyed.
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

    // The original document is actually gone. Snapshot values, not its path or
    // active owner, retain the backing/root until their final clone is released.
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
fn snapshot_retains_only_sources_in_current_root_and_releases_last_edit_arc() {
    // Fully replacing the original removes that backing from the current value,
    // even while the active document's history still owns its old roots.
    let (_directory, mut document) = fixture("原文");
    let original_mapping = Arc::downgrade(document.original_snapshot.as_ref().unwrap());
    document
        .replace_utf16_range(0..2, "只有编辑😀", MarkdownTextSelection::caret(0))
        .unwrap();
    let edited = document.text_snapshot();
    let edit_segment = Arc::downgrade(&document.edits.segments[0]);
    assert!(edited.text.original_snapshot.is_none());
    drop(document);

    // The fully replaced original belongs only to old history, so destroying
    // that history must release it independently of the current edited snapshot.
    assert!(original_mapping.upgrade().is_none());
    assert!(edit_segment.upgrade().is_some());
    assert_text(&edited, "只有编辑😀");
    drop(edited);
    assert!(edit_segment.upgrade().is_none());

    // The converse case abandons every large edit while retaining original text;
    // a fresh current value must not capture those history-only segment resources.
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
        // Observe actual destruction of each removed historical segment rather
        // than claiming release merely because the snapshot map is empty.
        assert!(segment.upgrade().is_none());
    }
    assert_text(&current, "保留原文");
}

#[test]
fn prepared_input_freeze_and_cancellation_preserve_all_published_values() {
    // Capture both before and during an unpublished native-input candidate;
    // neither immutable value may observe that candidate as committed text.
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

    // A newly captured value may share the segment containing an unpublished
    // suffix. Cancellation COW-restores only the live segment, never mutating
    // either frozen value or publishing the candidate root/history.
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
/// Keep a long input frozen under its stable segment ID across small input,
/// history, and snapshot ownership without rematerializing file bytes on the heap.
fn long_edit_keeps_frozen_segment_through_small_input_history_and_final_snapshot_drop() {
    let (_directory, mut document) = fixture("原");
    let input = "中😀a\r\n".repeat(32_000);
    document
        .replace_utf16_range(1..1, &input, MarkdownTextSelection::caret(1))
        .unwrap();
    let MarkdownEditSegment::Frozen(backing) = document.edits.segments[0].as_ref() else {
        // This real long edit must use the file-backed production path, not a
        // heap segment that happens to return the same characters.
        panic!("长输入应由真实匿名映射保管");
    };
    let observer = backing.observer();
    assert_eq!(observer.mapped_bytes(), input.len());
    assert_eq!(document.edits.allocated_bytes(), 0);
    let frozen = document.text_snapshot();
    // A later finite input may use a heap tail while the original long segment
    // remains the same shared frozen resource through undo and redo.
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
    // The last actual snapshot still owns the segment; document destruction
    // cannot truthfully claim its mapping already released.
    assert_eq!(observer.mapped_bytes(), input.len());
    assert_text(&frozen, &format!("原{input}"));
    drop(frozen);
    assert_eq!(observer.mapped_bytes(), 0);
    assert!(observer.unmap_succeeded());
}

#[test]
/// Cancellation first drops the unpublished candidate root, then rolls back
/// its original segment table while preserving current text and editing history.
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
    // Actual candidate bytes exist, but the current public snapshot remains
    // unchanged. Cancel must reclaim only that unpublished anonymous mapping.
    assert_text(&document.text_snapshot(), "原文");
    document.cancel_prepared_text_input(&token).unwrap();
    assert_eq!(document.storage_profile(), before);
    assert!(piece_tree_links_share_root(&document.root, &root));
    assert_eq!(observer.mapped_bytes(), 0);
    assert!(observer.unmap_succeeded());
    assert_text(&document.text_snapshot(), "原文");
}

#[test]
/// A recorded frozen-edit reclamation failure revokes cached observations and
/// later input preparation, even when the original document backing remains healthy.
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
    // This existing fault hook records EIO after a real page-reclamation call.
    // It exercises sticky propagation, not a naturally failing kernel operation.
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
    // Releasing the active owner alone is insufficient while a frozen value
    // still retains the same actual segment mapping.
    drop(document);
    assert!(observer.mapped_bytes() > 0);
    drop(frozen);
    assert!(observer.unmap_succeeded());
}

#[test]
fn bounded_visitor_borrows_actual_piece_bytes_and_stops_on_first_failure() {
    // A byte-range observation may split scalars but must still borrow the
    // actual mapped offsets and stop before later leaves after consumer failure.
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
            // Compare the actual mmap offset. Even partial UTF-8 scalar bytes
            // are neither copied nor interpreted as replacement characters.
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

    // Consumer failure stops the real traversal; a separately invalid request
    // fails before invoking that consumer at all.
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
    // Populate the shared cache before the existing intentional lock-poison
    // scenario; an old valid entry cannot conceal its later unavailable owner.
    let (_directory, document) = fixture("冻结原值😀");
    let snapshot = document.text_snapshot();
    assert_eq!(snapshot.utf16_code_unit_at(0), Ok(0x51bb));
    let worker = snapshot.clone();
    // Only this synthetic test deliberately panics while holding the actual
    // lock. Poison must not redefine a possibly partial window as valid text.
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
    // The same document and later independent captures share the actual poisoned
    // cache; rebuilding a clean cache would incorrectly bypass that failure.
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
    // Allocate one real read window, then repeatedly capture distinct frozen
    // values to detect per-capture cache allocation or unintended active-owner retention.
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
        // Every call creates a distinct FrozenText payload; using clone alone
        // could conceal an allocation mistakenly repeated only during capture.
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
    // Retained values own the same actual cache/backing after active teardown;
    // only releasing their final collection ends those resource lifetimes.
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
    // Alternate reads of old, edited, and undone values against a single cache
    // allocation; its key must retain each value's captured root and generation.
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
    // Undo can restore the exact same root allocation while its capture
    // generation differs; the active generation cannot substitute for a frozen key.
    assert!(Arc::ptr_eq(
        original.text.root.as_ref().unwrap(),
        restored.text.root.as_ref().unwrap()
    ));
    for _ in 0..128 {
        // Every read may replace only derived cache contents, never characters
        // or generation authority of the other immutable values.
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
    // Final active mutation and teardown likewise leave all saved values intact.
    assert_eq!(document.utf16_code_unit_at(0), Ok(0x65b0));
    drop(document);
    assert_text(&original, "旧😀文");
    assert_text(&changed, "新😀文");
    assert_text(&restored, "旧😀文");
}
