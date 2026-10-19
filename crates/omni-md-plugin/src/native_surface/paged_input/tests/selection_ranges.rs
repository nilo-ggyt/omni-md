//! 多选事实、输入和删除以同一个生产owner和真实Core校验，不拿第一范围冒充全集。

use super::*;

#[test]
fn multiple_ranges_keep_complete_set_and_do_not_invent_primary_direction() {
    let (_file, document) = fixture("a1 b2 c3", 4096);
    let memento = MarkdownTextSelection {
        anchor_utf16: 8,
        head_utf16: 3,
    };
    document.borrow_mut().set_selection(memento).unwrap();
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let current = |_| true;
    let before = owner.state().unwrap();
    let permit = owner.capture(1, &current).unwrap();
    let commit = committed(
        owner
            .set_selected_ranges(&permit, &[0..2, 6..8], &current)
            .unwrap(),
    );
    assert_eq!(commit.state.selection, memento); // 未知主方向不能改写原Core历史memento。
    assert_eq!(commit.state.directed_selection(), None);
    assert_eq!(commit.state.selection_range_count, 2);
    assert_eq!(commit.state.primary_range, NSRange::new(0, 2));
    assert_eq!(commit.state.generation, before.generation);
    assert_eq!(text(&document), "a1 b2 c3");
    assert!(!document.borrow().can_undo());
    let permit = owner.capture(1, &current).unwrap();
    assert_eq!(
        owner.selected_ranges(&permit, &current).unwrap(),
        vec![0..2, 6..8]
    );
    assert!(matches!(
        owner.insert_text(&permit, "X", unspecified(), &current),
        Err(NativeMarkdownPagedInputError::MultipleSelectionInput)
    ));
    assert_eq!(text(&document), "a1 b2 c3"); // 原生产Multiple输入没有唯一替换范围，不按平台首项静默修改。
    assert_eq!(owner.state().unwrap(), commit.state);
}

#[test]
fn invalid_or_excessive_ranges_keep_original_set_and_scalar_boundaries() {
    let (_file, document) = fixture("甲🙂尾", 4096);
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let current = |_| true;
    let permit = owner.capture(1, &current).unwrap();
    committed(
        owner
            .set_selected_ranges(&permit, &[0..1, 3..4], &current)
            .unwrap(),
    );
    let before = owner.state().unwrap();
    for ranges in [
        vec![0..1, 2..3],
        vec![3..4, 0..1],
        vec![0..3, 1..4],
        vec![0..0, 0..0],
        vec![0..1, 3..5],
        // 这是必须拒绝的倒置输入坐标，不是尝试反向迭代一个Rust Range。
        std::iter::once(Range { start: 3, end: 1 }).collect(),
        vec![0..0; NATIVE_MARKDOWN_PAGED_SELECTION_RANGE_LIMIT + 1],
    ] {
        let permit = owner.capture(1, &current).unwrap();
        assert!(
            owner
                .set_selected_ranges(&permit, &ranges, &current)
                .is_err()
        );
        assert_eq!(owner.state().unwrap(), before);
        let permit = owner.capture(1, &current).unwrap();
        assert_eq!(
            owner.selected_ranges(&permit, &current).unwrap(),
            vec![0..1, 3..4]
        );
    }
    assert_eq!(text(&document), "甲🙂尾");
    assert!(!document.borrow().can_undo());
}

#[test]
fn full_range_budget_and_middle_only_changes_revoke_old_ticket() {
    let (_file, document) = fixture(&"a ".repeat(1000), 4096);
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let current = |_| true;
    let mut ranges: Vec<_> = (0..1000).map(|index| index * 2..index * 2 + 1).collect();
    let permit = owner.capture(1, &current).unwrap();
    let commit = committed(
        owner
            .set_selected_ranges(&permit, &ranges, &current)
            .unwrap(),
    );
    assert_eq!(commit.state.selection_range_count, 1000);
    let stale = owner.capture(1, &current).unwrap();
    ranges[500] = 1000..1000;
    let permit = owner.capture(1, &current).unwrap();
    let next = committed(
        owner
            .set_selected_ranges(&permit, &ranges, &current)
            .unwrap(),
    );
    assert_eq!(next.state.primary_range, commit.state.primary_range);
    assert_eq!(
        next.state.selection_range_count,
        commit.state.selection_range_count
    );
    assert!(next.state.selection_serial > commit.state.selection_serial);
    assert!(owner.selected_ranges(&stale, &current).is_err());
    assert!(commit.check_current(&current).is_err());
    let permit = owner.capture(1, &current).unwrap();
    assert_eq!(owner.selected_ranges(&permit, &current).unwrap(), ranges);
    assert!(!document.borrow().can_undo());
}

#[test]
fn multiple_delete_is_one_atomic_transaction_and_keeps_unselected_middle_text() {
    let (_file, document) = fixture("a1 b2 c3", 4096);
    let memento = MarkdownTextSelection {
        anchor_utf16: 8,
        head_utf16: 3,
    };
    document.borrow_mut().set_selection(memento).unwrap();
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let current = |_| true;
    let permit = owner.capture(1, &current).unwrap();
    committed(
        owner
            .set_selected_ranges(&permit, &[0..2, 6..8], &current)
            .unwrap(),
    );
    let before = owner.state().unwrap();
    let permit = owner.capture(1, &current).unwrap();
    let commit = committed(owner.delete_selected_ranges(&permit, &current).unwrap());
    assert_eq!(text(&document), " b2 ");
    assert_eq!(commit.state.selection, MarkdownTextSelection::caret(4));
    assert_eq!(commit.state.generation, before.generation + 1);
    assert_eq!(commit.state.selection_range_count, 1);
    let permit = owner.capture(1, &current).unwrap();
    let undo = committed(
        owner
            .history(&permit, NativeMarkdownPagedInputHistory::Undo, &current)
            .unwrap(),
    );
    assert_eq!(text(&document), "a1 b2 c3");
    assert_eq!(undo.state.selection, memento);
    assert!(!document.borrow().can_undo());
    assert!(undo.state.selection_is_directed); // Core历史仍是明确单memento，没有伪称恢复多选集合。
}

#[test]
fn selected_bytes_delete_skips_empty_carets_and_all_empty_is_unchanged() {
    let (_file, document) = fixture("a1 b2 c3", 4096);
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let current = |_| true;
    let permit = owner.capture(1, &current).unwrap();
    committed(
        owner
            .set_selected_ranges(&permit, &[0..0, 6..8], &current)
            .unwrap(),
    );
    let permit = owner.capture(1, &current).unwrap();
    let commit = committed(owner.delete_selected_ranges(&permit, &current).unwrap());
    assert_eq!(text(&document), "a1 b2 ");
    assert_eq!(commit.state.selection, MarkdownTextSelection::caret(6));
    let permit = owner.capture(1, &current).unwrap();
    committed(
        owner
            .set_selected_ranges(&permit, &[0..0, 6..6], &current)
            .unwrap(),
    );
    let before = owner.state().unwrap();
    let permit = owner.capture(1, &current).unwrap();
    assert!(
        matches!(owner.delete_selected_ranges(&permit, &current).unwrap(), NativeMarkdownPagedInputOutcome::Unchanged(state) if state == before)
    );
    assert_eq!(owner.state().unwrap(), before);
}

#[test]
fn marked_single_selection_delete_retains_old_path_and_multi_replacement_is_rejected() {
    let (_file, document) = fixture("尾", 4096);
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let current = |_| true;
    let permit = owner.capture(1, &current).unwrap();
    owner
        .set_marked_text(
            &permit,
            "中🙂文",
            NSRange::new(1, 2),
            unspecified(),
            &current,
        )
        .unwrap();
    let before = owner.state().unwrap();
    let permit = owner.capture(1, &current).unwrap();
    assert!(matches!(
        owner.set_selected_ranges(&permit, &[0..1, 3..4], &current),
        Err(NativeMarkdownPagedInputError::CompositionActive)
    ));
    assert_eq!(owner.state().unwrap(), before);
    let permit = owner.capture(1, &current).unwrap();
    let deleted = committed(owner.delete_selected_ranges(&permit, &current).unwrap());
    assert_eq!(text(&document), "中文尾");
    assert_eq!(deleted.state.selection, MarkdownTextSelection::caret(1));
    assert_eq!(deleted.state.marked, None);
}

#[test]
fn explicit_replacement_always_publishes_single_caret_and_core_memento_history() {
    for (replacement, expected_text, caret) in [
        (NSRange::new(3, 2), "a1 Z c3", 4),
        (NSRange::new(2, 0), "a1Z b2 c3", 3),
        (NSRange::new(8, 0), "a1 b2 c3Z", 9),
        (NSRange::new(0, 2), "Z b2 c3", 1),
        (NSRange::new(1, 6), "aZ3", 2),
        (NSRange::new(6, 2), "a1 b2 Z", 7),
    ] {
        let (_file, document) = fixture("a1 b2 c3", 4096);
        let memento = MarkdownTextSelection {
            anchor_utf16: 8,
            head_utf16: 3,
        };
        document.borrow_mut().set_selection(memento).unwrap();
        let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
        let current = |_| true;
        let permit = owner.capture(1, &current).unwrap();
        committed(
            owner
                .set_selected_ranges(&permit, &[0..2, 6..8], &current)
                .unwrap(),
        );
        let permit = owner.capture(1, &current).unwrap();
        let commit = owner
            .insert_text(&permit, "Z", replacement, &current)
            .unwrap();
        assert_eq!(text(&document), expected_text);
        assert_eq!(
            commit.state.directed_selection(),
            Some(MarkdownTextSelection::caret(caret))
        );
        assert_eq!(commit.state.selection_range_count, 1);
        let permit = owner.capture(1, &current).unwrap();
        let undo = committed(
            owner
                .history(&permit, NativeMarkdownPagedInputHistory::Undo, &current)
                .unwrap(),
        );
        assert_eq!(text(&document), "a1 b2 c3");
        assert_eq!(undo.state.directed_selection(), Some(memento));
        assert_eq!(undo.state.selection_range_count, 1);
    }
}

#[test]
fn unspecified_marked_input_preserves_multiple_and_explicit_candidate_releases_range_metadata() {
    let (_file, document) = fixture("a1 b2 c3", 4096);
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let current = |_| true;
    let permit = owner.capture(1, &current).unwrap();
    committed(
        owner
            .set_selected_ranges(&permit, &[0..2, 6..8], &current)
            .unwrap(),
    );
    let permit = owner.capture(1, &current).unwrap();
    let before = owner.state().unwrap();
    assert!(matches!(
        owner.set_marked_text(&permit, "中", NSRange::new(1, 0), unspecified(), &current),
        Err(NativeMarkdownPagedInputError::MultipleSelectionInput)
    ));
    assert_eq!(owner.state().unwrap(), before);
    assert_eq!(text(&document), "a1 b2 c3");
    let permit = owner.capture(1, &current).unwrap();
    let commit = owner
        .set_marked_text(
            &permit,
            "中",
            NSRange::new(1, 0),
            NSRange::new(0, 2),
            &current,
        )
        .unwrap();
    assert_eq!(text(&document), "中 b2 c3");
    assert_eq!(commit.state.marked, Some(NSRange::new(0, 1)));
    assert_eq!(commit.state.selection_range_count, 1);
    assert!(commit.state.selection_is_directed);
    assert_eq!(owner.selection_ranges.borrow().capacity(), 0);
    let permit = owner.capture(1, &current).unwrap();
    committed(owner.unmark(&permit, &current).unwrap());
    let permit = owner.capture(1, &current).unwrap();
    committed(
        owner
            .set_selected_ranges(&permit, &[0..1, 5..7], &current)
            .unwrap(),
    );
    owner.close().unwrap();
    assert_eq!(owner.selection_ranges.borrow().capacity(), 0);
    assert!(commit.check_current(&current).is_err());
    assert_eq!(Rc::strong_count(&document), 1);
}
