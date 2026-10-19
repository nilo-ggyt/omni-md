//! 原field实际四组oracle的Core对照；额外输入类别只验证显式隔离，不推断新AppKit分组规则。

use super::*;

fn fixture(ime: bool) -> (tempfile::TempDir, MarkdownDocumentBuffer, MarkdownCellDraft) {
    let source = SOURCE.replacen("| a |", "| base |", 1);
    let (directory, mut doc) = document(&source);
    let token = begin(&mut doc);
    let edit = doc.capture_cell_draft_edit(&token).unwrap();
    let selection = if ime {
        MarkdownTextSelection {
            anchor_utf16: 0,
            head_utf16: 4,
        }
    } else {
        MarkdownTextSelection::caret(4)
    };
    doc.set_cell_draft_selection(edit, selection).unwrap();
    (directory, doc, token)
}

fn input(
    doc: &mut MarkdownDocumentBuffer,
    token: &MarkdownCellDraft,
    kind: MarkdownCellDraftInputKind,
    range: Range<usize>,
    text: &str,
    selected: MarkdownTextSelection,
    marked: Option<Range<usize>>,
) -> MarkdownCellDraftFacts {
    let edit = doc.capture_cell_draft_edit(token).unwrap();
    let intent = doc.capture_cell_draft_input_intent(&edit, kind).unwrap();
    doc.replace_cell_draft_grouped(edit, intent, range, text, selected, marked)
        .unwrap()
}

fn text(doc: &MarkdownDocumentBuffer, token: &MarkdownCellDraft) -> String {
    read(&doc.capture_cell_draft(token).unwrap())
}

fn history(
    doc: &mut MarkdownDocumentBuffer,
    token: &MarkdownCellDraft,
    direction: MarkdownCellDraftHistoryDirection,
) -> Option<MarkdownCellDraftHistoryEdit> {
    let edit = doc.capture_cell_draft_edit(token).unwrap();
    doc.restore_cell_draft_history(edit, direction).unwrap()
}

fn typed(doc: &mut MarkdownDocumentBuffer, token: &MarkdownCellDraft, value: &str) {
    let start = doc.cell_draft_facts(token).unwrap().selection.head_utf16;
    input(
        doc,
        token,
        MarkdownCellDraftInputKind::Typing,
        start..start,
        value,
        MarkdownTextSelection::caret(start + value.encode_utf16().count()),
        None,
    );
}

#[test]
fn native_ime_n_ni_commit_is_one_group_with_exact_original_selection_and_no_marked_history() {
    let (_dir, mut doc, token) = fixture(true);
    let canonical_before = canonical(&doc);
    let baseline = doc.storage_profile();
    input(
        &mut doc,
        &token,
        MarkdownCellDraftInputKind::Marked,
        0..4,
        "n",
        MarkdownTextSelection::caret(1),
        Some(0..1),
    );
    input(
        &mut doc,
        &token,
        MarkdownCellDraftInputKind::Marked,
        0..1,
        "ni",
        MarkdownTextSelection::caret(2),
        Some(0..2),
    );
    input(
        &mut doc,
        &token,
        MarkdownCellDraftInputKind::Typing,
        0..2,
        "你",
        MarkdownTextSelection::caret(1),
        None,
    );
    assert_eq!(text(&doc, &token), "你");
    let undo = history(&mut doc, &token, MarkdownCellDraftHistoryDirection::Undo).unwrap();
    assert_eq!(undo.replaced_utf16_range, 0..1);
    assert_eq!(undo.change_in_utf16_length, 3);
    assert_eq!(
        undo.facts.selection,
        MarkdownTextSelection {
            anchor_utf16: 0,
            head_utf16: 4
        }
    );
    assert_eq!(undo.facts.marked_range, None);
    assert_eq!(text(&doc, &token), "base");
    assert!(!doc.cell_draft_history_facts(&token).unwrap().can_undo);
    assert!(history(&mut doc, &token, MarkdownCellDraftHistoryDirection::Undo).is_none());
    let redo = history(&mut doc, &token, MarkdownCellDraftHistoryDirection::Redo).unwrap();
    assert_eq!(redo.replaced_utf16_range, 0..4);
    assert_eq!(redo.change_in_utf16_length, -3);
    assert_eq!(redo.facts.selection, MarkdownTextSelection::caret(1));
    assert_eq!(redo.facts.marked_range, None);
    assert_eq!(text(&doc, &token), "你");
    assert!(history(&mut doc, &token, MarkdownCellDraftHistoryDirection::Redo).is_none());
    assert_eq!(doc.storage_profile(), baseline);
    assert_eq!(canonical(&doc), canonical_before);
}

#[test]
fn native_contiguous_abc_is_one_group_and_redo_restores_the_final_caret() {
    let (_dir, mut doc, token) = fixture(false);
    for scalar in ["a", "b", "c"] {
        typed(&mut doc, &token, scalar);
    }
    assert_eq!(text(&doc, &token), "baseabc");
    let undo = history(&mut doc, &token, MarkdownCellDraftHistoryDirection::Undo).unwrap();
    assert_eq!(undo.replaced_utf16_range, 4..7);
    assert_eq!(undo.change_in_utf16_length, -3);
    assert_eq!(undo.facts.selection, MarkdownTextSelection::caret(4));
    assert_eq!(text(&doc, &token), "base");
    assert!(history(&mut doc, &token, MarkdownCellDraftHistoryDirection::Undo).is_none());
    let redo = history(&mut doc, &token, MarkdownCellDraftHistoryDirection::Redo).unwrap();
    assert_eq!(redo.replaced_utf16_range, 4..4);
    assert_eq!(redo.change_in_utf16_length, 3);
    assert_eq!(redo.facts.selection, MarkdownTextSelection::caret(7));
    assert_eq!(text(&doc, &token), "baseabc");
    assert!(history(&mut doc, &token, MarkdownCellDraftHistoryDirection::Redo).is_none());
}

#[test]
fn native_selection_move_breaks_ab_then_x_with_exact_two_group_selections() {
    let (_dir, mut doc, token) = fixture(false);
    typed(&mut doc, &token, "a");
    typed(&mut doc, &token, "b");
    let edit = doc.capture_cell_draft_edit(&token).unwrap();
    doc.set_cell_draft_selection(edit, MarkdownTextSelection::caret(0))
        .unwrap();
    typed(&mut doc, &token, "x");
    assert_eq!(text(&doc, &token), "xbaseab");
    for (expected, caret) in [("baseab", 0), ("base", 4)] {
        let restored = history(&mut doc, &token, MarkdownCellDraftHistoryDirection::Undo).unwrap();
        assert_eq!(
            restored.facts.selection,
            MarkdownTextSelection::caret(caret)
        );
        assert_eq!(restored.facts.marked_range, None);
        assert_eq!(text(&doc, &token), expected);
    }
    assert!(history(&mut doc, &token, MarkdownCellDraftHistoryDirection::Undo).is_none());
    for (expected, caret) in [("baseab", 6), ("xbaseab", 1)] {
        let restored = history(&mut doc, &token, MarkdownCellDraftHistoryDirection::Redo).unwrap();
        assert_eq!(
            restored.facts.selection,
            MarkdownTextSelection::caret(caret)
        );
        assert_eq!(text(&doc, &token), expected);
    }
    assert!(history(&mut doc, &token, MarkdownCellDraftHistoryDirection::Redo).is_none());
}

#[test]
fn recorded_group_end_selection_does_not_change_the_legacy_history_swap_contract() {
    for grouped in [false, true] {
        let (_dir, mut doc, token) = fixture(false);
        if grouped {
            typed(&mut doc, &token, "a");
        } else {
            edit(
                &mut doc,
                &token,
                4..4,
                "a",
                MarkdownTextSelection::caret(5),
                None,
            )
            .unwrap();
        }
        let edit = doc.capture_cell_draft_edit(&token).unwrap();
        doc.set_cell_draft_selection(edit, MarkdownTextSelection::caret(0))
            .unwrap();
        let restored = history(&mut doc, &token, MarkdownCellDraftHistoryDirection::Undo).unwrap();
        assert_eq!(restored.facts.selection, MarkdownTextSelection::caret(4));
        let restored = history(&mut doc, &token, MarkdownCellDraftHistoryDirection::Redo).unwrap();
        // 只有新显式分组固定记录末态；legacy仍交换Undo前的临时选区，防止顺带改写旧接口。
        assert_eq!(
            restored.facts.selection,
            MarkdownTextSelection::caret(if grouped { 5 } else { 0 })
        );
        assert_eq!(text(&doc, &token), "basea");
    }
}

#[test]
fn native_explicit_coalescing_break_is_state_only_and_splits_ab_from_c() {
    let (_dir, mut doc, token) = fixture(false);
    typed(&mut doc, &token, "a");
    typed(&mut doc, &token, "b");
    let before = doc.cell_draft_facts(&token).unwrap();
    let edit = doc.capture_cell_draft_edit(&token).unwrap();
    let after = doc.break_cell_draft_undo_coalescing(edit).unwrap();
    assert!(after.serial > before.serial);
    assert_eq!(after.content_serial, before.content_serial);
    assert_eq!(after.selection, before.selection);
    typed(&mut doc, &token, "c");
    for (expected, caret) in [("baseab", 6), ("base", 4)] {
        let restored = history(&mut doc, &token, MarkdownCellDraftHistoryDirection::Undo).unwrap();
        assert_eq!(
            restored.facts.selection,
            MarkdownTextSelection::caret(caret)
        );
        assert_eq!(text(&doc, &token), expected);
    }
    assert!(history(&mut doc, &token, MarkdownCellDraftHistoryDirection::Undo).is_none());
    for (expected, caret) in [("baseab", 6), ("baseabc", 7)] {
        let restored = history(&mut doc, &token, MarkdownCellDraftHistoryDirection::Redo).unwrap();
        assert_eq!(
            restored.facts.selection,
            MarkdownTextSelection::caret(caret)
        );
        assert_eq!(text(&doc, &token), expected);
    }
    assert!(history(&mut doc, &token, MarkdownCellDraftHistoryDirection::Redo).is_none());
}

#[test]
fn legacy_entry_keeps_each_transaction_and_isolated_intent_does_not_guess_paste_coalescing() {
    let (_dir, mut doc, token) = fixture(false);
    for scalar in ["a", "b", "c"] {
        let at = doc.cell_draft_facts(&token).unwrap().utf16_len;
        edit(
            &mut doc,
            &token,
            at..at,
            scalar,
            MarkdownTextSelection::caret(at + 1),
            None,
        )
        .unwrap();
    }
    let mut count = 0;
    while history(&mut doc, &token, MarkdownCellDraftHistoryDirection::Undo).is_some() {
        count += 1;
    }
    assert_eq!(count, 3);
    assert_eq!(text(&doc, &token), "base");
    input(
        &mut doc,
        &token,
        MarkdownCellDraftInputKind::Isolated,
        4..4,
        "ab",
        MarkdownTextSelection::caret(6),
        None,
    );
    typed(&mut doc, &token, "c");
    history(&mut doc, &token, MarkdownCellDraftHistoryDirection::Undo).unwrap();
    assert_eq!(text(&doc, &token), "baseab");
    history(&mut doc, &token, MarkdownCellDraftHistoryDirection::Undo).unwrap();
    assert_eq!(text(&doc, &token), "base");
    assert!(history(&mut doc, &token, MarkdownCellDraftHistoryDirection::Undo).is_none());
}

#[test]
fn failed_payload_preserves_the_existing_group_root_delta_and_redo_fork() {
    let (_dir, mut doc, token) = fixture(false);
    typed(&mut doc, &token, "a");
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let before = doc.cell_draft_facts(&token).unwrap();
    let delta = observer.snapshot().delta_bytes;
    let edit = doc.capture_cell_draft_edit(&token).unwrap();
    let intent = doc
        .capture_cell_draft_input_intent(&edit, MarkdownCellDraftInputKind::Marked)
        .unwrap();
    assert!(matches!(
        doc.replace_cell_draft_grouped(
            edit,
            intent,
            5..5,
            "x",
            MarkdownTextSelection::caret(6),
            None
        ),
        Err(MarkdownCellDraftError::InvalidInputIntent)
    ));
    assert_eq!(doc.cell_draft_facts(&token).unwrap(), before);
    assert_eq!(observer.snapshot().delta_bytes, delta);
    typed(&mut doc, &token, "b");
    history(&mut doc, &token, MarkdownCellDraftHistoryDirection::Undo).unwrap();
    assert_eq!(text(&doc, &token), "base");
    assert!(history(&mut doc, &token, MarkdownCellDraftHistoryDirection::Undo).is_none());
    typed(&mut doc, &token, "z");
    assert!(!doc.cell_draft_history_facts(&token).unwrap().can_redo);
}

#[test]
fn intent_is_bound_to_the_original_scope_and_serial_even_with_a_new_valid_edit_capsule() {
    let (_dir, mut doc, token) = fixture(false);
    let (_other_dir, other, other_token) = fixture(false);
    let foreign_edit = other.capture_cell_draft_edit(&other_token).unwrap();
    let foreign_intent = other
        .capture_cell_draft_input_intent(&foreign_edit, MarkdownCellDraftInputKind::Typing)
        .unwrap();
    let edit = doc.capture_cell_draft_edit(&token).unwrap();
    assert!(matches!(
        doc.replace_cell_draft_grouped(
            edit,
            foreign_intent,
            4..4,
            "x",
            MarkdownTextSelection::caret(5),
            None
        ),
        Err(MarkdownCellDraftError::Stale)
    ));
    let old_edit = doc.capture_cell_draft_edit(&token).unwrap();
    let old_intent = doc
        .capture_cell_draft_input_intent(&old_edit, MarkdownCellDraftInputKind::Typing)
        .unwrap();
    typed(&mut doc, &token, "a");
    let new_edit = doc.capture_cell_draft_edit(&token).unwrap();
    assert!(matches!(
        doc.replace_cell_draft_grouped(
            new_edit,
            old_intent,
            5..5,
            "x",
            MarkdownTextSelection::caret(6),
            None
        ),
        Err(MarkdownCellDraftError::Stale)
    ));
    typed(&mut doc, &token, "b");
    history(&mut doc, &token, MarkdownCellDraftHistoryDirection::Undo).unwrap();
    assert_eq!(text(&doc, &token), "base");
    assert!(history(&mut doc, &token, MarkdownCellDraftHistoryDirection::Undo).is_none());
}

#[test]
fn group_budget_is_still_128_roots_and_history_does_not_refund_cumulative_delta() {
    let (_dir, mut doc, token) = fixture(false);
    for _ in 0..130 {
        typed(&mut doc, &token, "x");
        typed(&mut doc, &token, "x");
        let edit = doc.capture_cell_draft_edit(&token).unwrap();
        doc.break_cell_draft_undo_coalescing(edit).unwrap();
    }
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let delta = observer.snapshot().delta_bytes;
    let mut count = 0;
    while history(&mut doc, &token, MarkdownCellDraftHistoryDirection::Undo).is_some() {
        count += 1;
    }
    assert_eq!(count, 128);
    assert_eq!(text(&doc, &token), "basexxxx");
    assert_eq!(observer.snapshot().delta_bytes, delta);
}

#[test]
fn group_source_errno_precedes_bad_intent_and_cancel_without_publishing_or_hiding_resources() {
    let (_dir, mut doc, token) = fixture(false);
    typed(&mut doc, &token, "a");
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let backing = doc.original_backing_observer().unwrap();
    let before = doc.cell_draft_facts(&token).unwrap();
    let edit = doc.capture_cell_draft_edit(&token).unwrap();
    let intent = doc
        .capture_cell_draft_input_intent(&edit, MarkdownCellDraftInputKind::Marked)
        .unwrap();
    doc.original_snapshot
        .as_ref()
        .unwrap()
        .fail_next_reclaim_for_test();
    assert_eq!(
        doc.reclaim_original_read_windows_for_test(),
        Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
            libc::EIO
        ))
    );
    assert!(matches!(
        doc.replace_cell_draft_grouped(
            edit,
            intent,
            5..5,
            "x",
            MarkdownTextSelection::caret(6),
            None
        ),
        Err(MarkdownCellDraftError::Document(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
    let Some(CellDraftSlot::Active(pending)) = &doc.cell_draft else {
        panic!("原草稿仍须存在")
    };
    assert_eq!(pending.facts(), before);
    assert_eq!(observer.snapshot().first_source_errno, Some(libc::EIO));
    assert!(doc.cancel_cell_draft(&token));
    drop(doc);
    assert!(observer.is_released());
    assert!(backing.unmap_succeeded());
    assert!(backing.failed());
}

#[test]
fn explicit_unicode_group_uses_utf16_geometry_and_rejected_surrogate_does_not_break_the_group() {
    let (_dir, mut doc, token) = fixture(false);
    typed(&mut doc, &token, "😀");
    let before = doc.cell_draft_facts(&token).unwrap();
    let edit = doc.capture_cell_draft_edit(&token).unwrap();
    let intent = doc
        .capture_cell_draft_input_intent(&edit, MarkdownCellDraftInputKind::Typing)
        .unwrap();
    assert!(
        doc.replace_cell_draft_grouped(
            edit,
            intent,
            5..5,
            "x",
            MarkdownTextSelection::caret(6),
            None
        )
        .is_err()
    );
    assert_eq!(doc.cell_draft_facts(&token).unwrap(), before);
    typed(&mut doc, &token, "中");
    let undo = history(&mut doc, &token, MarkdownCellDraftHistoryDirection::Undo).unwrap();
    assert_eq!(undo.replaced_utf16_range, 4..7);
    assert_eq!(undo.change_in_utf16_length, -3);
    assert_eq!(text(&doc, &token), "base");
    assert!(history(&mut doc, &token, MarkdownCellDraftHistoryDirection::Undo).is_none());
    let redo = history(&mut doc, &token, MarkdownCellDraftHistoryDirection::Redo).unwrap();
    assert_eq!(redo.facts.selection, MarkdownTextSelection::caret(7));
    assert_eq!(text(&doc, &token), "base😀中");
}

#[test]
fn unfinished_composition_rejects_history_or_explicit_break_without_consuming_the_original_group() {
    let (_dir, mut doc, token) = fixture(true);
    input(
        &mut doc,
        &token,
        MarkdownCellDraftInputKind::Marked,
        0..4,
        "n",
        MarkdownTextSelection::caret(1),
        Some(0..1),
    );
    let before = doc.cell_draft_facts(&token).unwrap();
    let edit = doc.capture_cell_draft_edit(&token).unwrap();
    assert_eq!(
        doc.break_cell_draft_undo_coalescing(edit),
        Err(MarkdownCellDraftError::CompositionActive)
    );
    let edit = doc.capture_cell_draft_edit(&token).unwrap();
    assert_eq!(
        doc.restore_cell_draft_history(edit, MarkdownCellDraftHistoryDirection::Undo),
        Err(MarkdownCellDraftError::CompositionActive)
    );
    assert_eq!(doc.cell_draft_facts(&token).unwrap(), before);
    input(
        &mut doc,
        &token,
        MarkdownCellDraftInputKind::Typing,
        0..1,
        "你",
        MarkdownTextSelection::caret(1),
        None,
    );
    history(&mut doc, &token, MarkdownCellDraftHistoryDirection::Undo).unwrap();
    assert_eq!(text(&doc, &token), "base");
    assert!(history(&mut doc, &token, MarkdownCellDraftHistoryDirection::Undo).is_none());
}
