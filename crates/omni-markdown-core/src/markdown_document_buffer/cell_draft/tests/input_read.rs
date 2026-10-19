//! 同一原输入票的有限窗口、坐标域与真实读取名额；不另建冻结root伪造低内存。

use super::*;

#[test]
fn input_reads_reuse_the_one_doc_cache_with_two_real_layout_readers_and_distinct_source_domain() {
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(&mut doc, &token, "中😀x");
    let first = doc.capture_cell_draft(&token).unwrap();
    let second = doc.capture_cell_draft(&token).unwrap();
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    assert_eq!(observer.snapshot().read_roots, 2);
    // 先填canonical缓存，再用相同Doc的草稿读取；不能把已有正文窗口当作局部草稿结果。
    let mut canonical_units = [0; 4];
    doc.copy_utf16_code_units(0, &mut canonical_units).unwrap();
    let cache = doc
        .utf16_read_cache
        .lock()
        .unwrap()
        .allocated_window_for_test()
        .unwrap();
    let edit = doc.capture_cell_draft_edit(&token).unwrap();
    let (facts, authority) = doc.cell_draft_edit_facts(&edit).unwrap();
    assert_eq!(facts.utf16_len, 4);
    assert!(authority == first.source_authority(&first.authority()).unwrap());
    let mut units = [0; 4];
    doc.copy_cell_draft_utf16_code_units(&edit, 0, &mut units)
        .unwrap();
    assert_eq!(units.as_slice(), "中😀x".encode_utf16().collect::<Vec<_>>());
    assert_ne!(units, canonical_units);
    assert_eq!(
        doc.utf16_read_cache
            .lock()
            .unwrap()
            .allocated_window_for_test()
            .unwrap(),
        cache
    );
    assert_eq!(observer.snapshot().read_roots, 2); // 输入没有capture Frozen或偷占第三个读root。
    assert_eq!(canonical(&doc), SOURCE);
    drop((first, second));
    assert_eq!(observer.snapshot().read_roots, 0);
    assert!(doc.cancel_cell_draft(&token));
    assert!(!observer.snapshot().branch_live);
}

#[test]
fn scalar_range_and_nsstring_unit_reads_keep_the_original_serial_and_finite_limit() {
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(&mut doc, &token, "中😀x");
    let edit = doc.capture_cell_draft_edit(&token).unwrap();
    doc.validate_cell_draft_utf16_range(&edit, 1..3).unwrap();
    assert!(doc.validate_cell_draft_utf16_range(&edit, 2..3).is_err());
    let mut low = [0];
    doc.copy_cell_draft_utf16_code_units(&edit, 2, &mut low)
        .unwrap();
    assert!((0xdc00..=0xdfff).contains(&low[0])); // NSString允许读取半代理单元；编辑范围仍不允许。
    doc.copy_cell_draft_utf16_code_units(&edit, 4, &mut [])
        .unwrap();
    assert!(
        doc.copy_cell_draft_utf16_code_units(&edit, 5, &mut [])
            .is_err()
    );
    assert_eq!(
        doc.copy_cell_draft_utf16_code_units(&edit, 0, &mut [0; 4097]),
        Err(MarkdownCellDraftError::ReadLimit)
    );
    let (_other_dir, other) = document(SOURCE);
    assert!(matches!(
        other.cell_draft_edit_facts(&edit),
        Err(MarkdownCellDraftError::Stale)
    ));
    let selection_ticket = doc.capture_cell_draft_edit(&token).unwrap();
    doc.set_cell_draft_selection(selection_ticket, MarkdownTextSelection::caret(0))
        .unwrap();
    assert!(matches!(
        doc.cell_draft_edit_facts(&edit),
        Err(MarkdownCellDraftError::Stale)
    ));
    assert!(matches!(
        doc.copy_cell_draft_utf16_code_units(&edit, 0, &mut low),
        Err(MarkdownCellDraftError::Stale)
    ));
    assert!(doc.cancel_cell_draft(&token));
}

#[test]
fn actual_source_error_wins_over_cached_or_empty_input_reads_and_stale_serial() {
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(&mut doc, &token, "中😀x");
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let stale = doc.capture_cell_draft_edit(&token).unwrap();
    let mut unit = [0];
    doc.copy_cell_draft_utf16_code_units(&stale, 0, &mut unit)
        .unwrap();
    let current = doc.capture_cell_draft_edit(&token).unwrap();
    doc.set_cell_draft_selection(current, MarkdownTextSelection::caret(0))
        .unwrap();
    doc.original_snapshot
        .as_ref()
        .unwrap()
        .fail_next_reclaim_for_test();
    let error = MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO);
    assert_eq!(doc.reclaim_original_read_windows_for_test(), Err(error));
    for empty in [false, true] {
        let output = if empty { &mut [][..] } else { &mut unit[..] };
        assert_eq!(
            doc.copy_cell_draft_utf16_code_units(&stale, 0, output),
            Err(error.into())
        );
    }
    assert!(
        matches!(doc.cell_draft_edit_facts(&stale), Err(MarkdownCellDraftError::Document(actual)) if actual == error)
    );
    assert_eq!(
        doc.validate_cell_draft_utf16_range(&stale, 0..0),
        Err(error.into())
    );
    assert_eq!(observer.snapshot().first_source_errno, Some(libc::EIO));
}
