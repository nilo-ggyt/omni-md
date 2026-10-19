//! 同一真实选择槽的意图与几何事实；不借NSUndoManager或假Native布局建立历史能力。

use super::*;

fn goal() -> NativeMarkdownVerticalGoal {
    NativeMarkdownVerticalGoal::new(38.5, 74.25, 640 * 64, 19).unwrap()
}

fn facts(
    owner: &Rc<NativeMarkdownPagedInput>,
) -> (Option<bool>, Option<NativeMarkdownVerticalGoal>) {
    let permit = owner.capture(1, &|epoch| epoch == 1).unwrap();
    owner
        .navigation_selection_facts(&permit, &|epoch| epoch == 1)
        .unwrap()
}

fn bidi_side(owner: &Rc<NativeMarkdownPagedInput>) -> Option<NativeMarkdownBidiCaretSide> {
    let permit = owner.capture(1, &|_| true).unwrap();
    owner.navigation_bidi_side(&permit, &|_| true).unwrap()
}

#[test]
fn same_head_bidi_side_uses_one_selection_receipt_and_is_cleared_by_real_edits_history_and_geometry()
 {
    let (_file, document) = fixture("aאבb", 4096);
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let before = owner.state().unwrap();
    let first_side = NativeMarkdownBidiCaretSide {
        level: 0,
        logical_after: true,
    };
    let other_side = NativeMarkdownBidiCaretSide {
        level: 1,
        logical_after: false,
    };
    let permit = owner.capture(1, &|_| true).unwrap();
    let first = committed(
        owner
            .set_visual_selection_intent(
                &permit,
                MarkdownTextSelection::caret(3),
                false,
                first_side,
                &|_| true,
            )
            .unwrap(),
    );
    let stale = owner.capture(1, &|_| true).unwrap();
    let permit = owner.capture(1, &|_| true).unwrap();
    let other = committed(
        owner
            .set_visual_selection_intent(
                &permit,
                MarkdownTextSelection::caret(3),
                false,
                other_side,
                &|_| true,
            )
            .unwrap(),
    );
    assert_eq!(other.state.selection, first.state.selection);
    assert_eq!(other.state.generation, before.generation);
    assert_eq!(
        other.state.selection_serial,
        first.state.selection_serial + 1
    );
    assert_eq!(bidi_side(&owner), Some(other_side));
    assert!(other.invalidation.selection_changed && !other.invalidation.characters_changed);
    assert!(!document.borrow().can_undo() && !document.borrow().can_redo());
    assert!(matches!(
        owner.set_visual_selection_intent(
            &stale,
            MarkdownTextSelection::caret(2),
            false,
            first_side,
            &|_| true
        ),
        Err(NativeMarkdownPagedInputError::StateChanged)
    ));
    // 真文字提交清旧侧，Undo恢复文字/方向也不能复用原Font/行布局的part票。
    let permit = owner.capture(1, &|_| true).unwrap();
    owner
        .insert_text(&permit, "X", NSRange::new(3, 0), &|_| true)
        .unwrap();
    assert_eq!(bidi_side(&owner), None);
    let permit = owner.capture(1, &|_| true).unwrap();
    owner
        .history(&permit, NativeMarkdownPagedInputHistory::Undo, &|_| true)
        .unwrap();
    assert_eq!(text(&document), "aאבb");
    assert_eq!(bidi_side(&owner), None);
    let permit = owner.capture(1, &|_| true).unwrap();
    committed(
        owner
            .set_visual_selection_intent(
                &permit,
                MarkdownTextSelection::caret(3),
                false,
                other_side,
                &|_| true,
            )
            .unwrap(),
    );
    owner.clear_navigation_geometry();
    assert_eq!(bidi_side(&owner), None);
}

#[test]
fn rejected_or_reentrant_visual_side_does_not_replace_the_original_selection() {
    let (_file, document) = fixture("aאבb", 4096);
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let side = NativeMarkdownBidiCaretSide {
        level: 1,
        logical_after: false,
    };
    let permit = owner.capture(1, &|_| true).unwrap();
    committed(
        owner
            .set_visual_selection_intent(
                &permit,
                MarkdownTextSelection::caret(3),
                false,
                side,
                &|_| true,
            )
            .unwrap(),
    );
    let before = owner.state().unwrap();
    let permit = owner.capture(1, &|_| true).unwrap();
    assert!(matches!(
        owner.set_visual_selection_intent(
            &permit,
            MarkdownTextSelection::caret(2),
            false,
            NativeMarkdownBidiCaretSide {
                level: 127,
                logical_after: true
            },
            &|_| true
        ),
        Err(NativeMarkdownPagedInputError::InvalidRange)
    ));
    assert_eq!(owner.state().unwrap(), before);
    assert_eq!(bidi_side(&owner), Some(side));
    let permit = owner.capture(1, &|_| true).unwrap();
    let calls = Cell::new(0);
    assert!(matches!(
        owner.navigation_bidi_side(&permit, &|_| {
            calls.set(calls.get() + 1);
            if calls.get() == 3 {
                owner.clear_navigation_geometry();
            }
            true
        }),
        Err(NativeMarkdownPagedInputError::StateChanged)
    ));
    assert_eq!(bidi_side(&owner), None);
    assert_eq!(owner.state().unwrap(), before);
    assert_eq!(text(&document), "aאבb");
    assert!(!document.borrow().can_undo());
}

#[test]
fn same_range_intent_publishes_new_side_and_goal_without_text_or_history() {
    let (_file, document) = fixture("long short long", 4096);
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let initial = owner.state().unwrap();
    let original = owner.capture(1, &|_| true).unwrap();
    assert!(
        matches!(owner.set_selection(&original, initial.selection, &|_| true).unwrap(), NativeMarkdownPagedInputOutcome::Unchanged(state) if state == initial)
    );
    let permit = owner.capture(1, &|_| true).unwrap();
    let first = committed(
        owner
            .set_selection_intent(
                &permit,
                initial.selection,
                Some(true),
                Some(goal()),
                &|_| true,
            )
            .unwrap(),
    );
    assert_eq!(first.state.selection, initial.selection);
    assert_eq!(first.state.generation, initial.generation);
    assert_eq!(first.state.selection_serial, initial.selection_serial + 1);
    assert_eq!(facts(&owner), (Some(true), Some(goal())));
    assert!(first.invalidation.selection_changed && !first.invalidation.characters_changed);
    let next = owner.capture(1, &|_| true).unwrap();
    let second = committed(
        owner
            .set_selection_intent(&next, initial.selection, Some(false), None, &|_| true)
            .unwrap(),
    );
    assert_eq!(
        second.state.selection_serial,
        first.state.selection_serial + 1
    );
    assert_eq!(facts(&owner), (Some(false), None));
    assert_eq!(text(&document), "long short long");
    assert!(!document.borrow().can_undo() && !document.borrow().can_redo());
    assert!(matches!(
        first.check_current(&|_| true),
        Err(NativeMarkdownPagedInputError::StateChanged)
    ));
}

#[test]
fn short_line_target_keeps_original_goal_and_scalar_failure_preserves_the_same_slot() {
    let (_file, document) = fixture("long\n短\nlong🙂", 4096);
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    // 本测试只验证真实输入发布；目标位置和列均为显式有限测试值，不声称Native导航结果。
    for position in [3, 5, 9] {
        let permit = owner.capture(1, &|_| true).unwrap();
        committed(
            owner
                .set_selection_intent(
                    &permit,
                    MarkdownTextSelection::caret(position),
                    Some(false),
                    Some(goal()),
                    &|_| true,
                )
                .unwrap(),
        );
        assert_eq!(facts(&owner), (Some(false), Some(goal())));
    }
    let stable = owner.state().unwrap();
    let bad = owner.capture(1, &|_| true).unwrap();
    assert!(
        owner
            .set_selection_intent(
                &bad,
                MarkdownTextSelection::caret(12),
                Some(true),
                None,
                &|_| true
            )
            .is_err()
    );
    assert_eq!(owner.state().unwrap(), stable);
    assert_eq!(facts(&owner), (Some(false), Some(goal())));
    assert_eq!(text(&document), "long\n短\nlong🙂");
    assert!(!document.borrow().can_undo());
}

#[test]
fn unknown_side_goal_and_revoked_intent_cannot_replace_existing_facts() {
    let (_file, document) = fixture("abc", 4096);
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let permit = owner.capture(1, &|_| true).unwrap();
    committed(
        owner
            .set_selection_intent(
                &permit,
                MarkdownTextSelection::caret(1),
                Some(true),
                Some(goal()),
                &|_| true,
            )
            .unwrap(),
    );
    let stable = owner.state().unwrap();
    let invalid = owner.capture(1, &|_| true).unwrap();
    assert!(matches!(
        owner.set_selection_intent(
            &invalid,
            MarkdownTextSelection::caret(2),
            None,
            Some(goal()),
            &|_| true
        ),
        Err(NativeMarkdownPagedInputError::InvalidRange)
    ));
    let revoked = owner.capture(1, &|_| true).unwrap();
    assert!(matches!(
        owner.set_selection_intent(
            &revoked,
            MarkdownTextSelection::caret(2),
            Some(false),
            None,
            &|_| false
        ),
        Err(NativeMarkdownPagedInputError::Revoked)
    ));
    assert_eq!(owner.state().unwrap(), stable);
    assert_eq!(facts(&owner), (Some(true), Some(goal())));
}

#[test]
fn reentrant_geometry_clear_cannot_resurrect_pre_callback_goal() {
    let (_file, document) = fixture("abc", 4096);
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let permit = owner.capture(1, &|_| true).unwrap();
    committed(
        owner
            .set_selection_intent(
                &permit,
                MarkdownTextSelection::caret(1),
                Some(false),
                Some(goal()),
                &|_| true,
            )
            .unwrap(),
    );
    let stable = owner.state().unwrap();
    let permit = owner.capture(1, &|_| true).unwrap();
    let calls = Cell::new(0usize);
    let result = owner.navigation_selection_facts(&permit, &|_| {
        calls.set(calls.get() + 1);
        if calls.get() == 3 {
            owner.clear_navigation_geometry();
        } // 第二轮原票核准中的真实重入；canonical状态不变，几何事实已经明确撤销。
        true
    });
    assert!(matches!(
        result,
        Err(NativeMarkdownPagedInputError::StateChanged)
    ));
    assert_eq!(owner.state().unwrap(), stable);
    assert_eq!(facts(&owner), (None, None));
}

#[test]
fn text_history_multi_selection_and_layout_clear_do_not_inherit_old_goal() {
    let (_file, document) = fixture("abcd", 4096);
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let permit = owner.capture(1, &|_| true).unwrap();
    committed(
        owner
            .set_selection_intent(
                &permit,
                MarkdownTextSelection::caret(1),
                Some(true),
                Some(goal()),
                &|_| true,
            )
            .unwrap(),
    );
    let stable = owner.state().unwrap();
    owner.clear_navigation_geometry();
    assert_eq!(owner.state().unwrap(), stable);
    assert_eq!(facts(&owner), (None, None));
    let permit = owner.capture(1, &|_| true).unwrap();
    committed(
        owner
            .set_selection_intent(
                &permit,
                stable.selection,
                Some(false),
                Some(goal()),
                &|_| true,
            )
            .unwrap(),
    );
    let permit = owner.capture(1, &|_| true).unwrap();
    owner
        .insert_text(&permit, "x", unspecified(), &|_| true)
        .unwrap();
    assert_eq!(facts(&owner), (None, None));
    let permit = owner.capture(1, &|_| true).unwrap();
    committed(
        owner
            .history(&permit, NativeMarkdownPagedInputHistory::Undo, &|_| true)
            .unwrap(),
    );
    assert_eq!(text(&document), "abcd");
    assert_eq!(facts(&owner), (None, None));
    let permit = owner.capture(1, &|_| true).unwrap();
    committed(
        owner
            .set_selected_ranges(&permit, &[0..1, 2..3], &|_| true)
            .unwrap(),
    );
    assert!(!owner.state().unwrap().selection_is_directed);
    assert_eq!(facts(&owner), (None, None));
    assert!(!document.borrow().can_undo() && document.borrow().can_redo());
}
