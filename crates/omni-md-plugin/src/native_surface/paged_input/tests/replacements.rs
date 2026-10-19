//! 批量替换直接消费生产入口，核对唯一历史、失败回滚和原选择身份。

use super::*;
use omni_markdown_core::{MarkdownReplacement, MarkdownReplacementPart};

type LiteralReplacement = MarkdownReplacement<
    [std::result::Result<MarkdownReplacementPart<'static>, MarkdownDocumentBufferError>; 1],
>;

fn literal(
    range: Range<usize>,
    text: &'static str,
) -> std::result::Result<LiteralReplacement, MarkdownDocumentBufferError> {
    Ok(MarkdownReplacement {
        range_utf16: range,
        parts: [Ok(MarkdownReplacementPart::Literal(text.into()))],
    })
}

#[test]
fn batch_uses_complete_reversed_before_and_exactly_one_undo_redo_entry() {
    let (_file, document) = fixture("甲🙂 甲🙂", 4096);
    let before = MarkdownTextSelection {
        anchor_utf16: 7,
        head_utf16: 1,
    };
    document.borrow_mut().set_selection(before).unwrap();
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let state = owner.state().unwrap();
    let current = |_| true;
    let permit = owner.capture(5, &current).unwrap();
    let after = MarkdownTextSelection {
        anchor_utf16: 4,
        head_utf16: 0,
    };
    let commit = committed(
        owner
            .apply_replacements_observed(
                &permit,
                [literal(0..3, "x"), literal(4..7, "中文")],
                after,
                &current,
            )
            .unwrap(),
    );
    assert_eq!(text(&document), "x 中文");
    assert_eq!(commit.state.generation, state.generation + 1);
    assert_eq!(commit.state.selection, after);
    assert_eq!(commit.invalidation.replaced_utf16_range, Some(0..7));
    assert_eq!(commit.invalidation.change_in_utf16_length, -3);
    assert!(matches!(
        owner.apply_replacements_observed(
            &permit,
            [literal(0..1, "不得重放")],
            MarkdownTextSelection::caret(0),
            &current,
        ),
        Err(NativeMarkdownPagedInputError::Consumed)
    ));
    let permit = owner.capture(5, &current).unwrap();
    let restored = committed(
        owner
            .history(&permit, NativeMarkdownPagedInputHistory::Undo, &current)
            .unwrap(),
    );
    assert_eq!(text(&document), "甲🙂 甲🙂");
    assert_eq!(restored.state.selection, before);
    assert!(!document.borrow().can_undo());
    let permit = owner.capture(5, &current).unwrap();
    let restored = committed(
        owner
            .history(&permit, NativeMarkdownPagedInputHistory::Redo, &current)
            .unwrap(),
    );
    assert_eq!(text(&document), "x 中文");
    assert_eq!(restored.state.selection, after);
    assert!(!document.borrow().can_redo());
}

#[test]
fn batch_invalid_stream_order_scalar_and_capacity_roll_back_everything() {
    let (_file, document) = fixture("甲🙂尾", 32);
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let current = |_| true;
    let before = owner.state().unwrap();
    let cases = [
        vec![
            literal(0..1, "先追加"),
            Err(MarkdownDocumentBufferError::DocumentTooLarge),
        ],
        vec![literal(3..4, "尾"), literal(0..1, "甲")],
        vec![literal(0..1, "x"), literal(2..3, "低代理非法")],
        vec![literal(0..1, "本次真实新增长度必定超过原配置容量")],
    ];
    for replacements in cases {
        let permit = owner.capture(1, &current).unwrap();
        assert!(
            owner
                .apply_replacements_observed(
                    &permit,
                    replacements,
                    MarkdownTextSelection::caret(0),
                    &current,
                )
                .is_err()
        );
        assert_eq!(owner.state().unwrap(), before);
        assert_eq!(text(&document), "甲🙂尾");
        assert!(!document.borrow().can_undo());
        assert!(!document.borrow().has_prepared_text_input());
        assert!(!owner.is_busy());
    }
}

#[test]
fn empty_batch_preserves_selection_generation_and_existing_redo() {
    let (_file, document) = fixture("原文", 4096);
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let current = |_| true;
    let permit = owner.capture(1, &current).unwrap();
    owner
        .insert_text(&permit, "x", unspecified(), &current)
        .unwrap();
    let permit = owner.capture(1, &current).unwrap();
    committed(
        owner
            .history(&permit, NativeMarkdownPagedInputHistory::Undo, &current)
            .unwrap(),
    );
    assert!(document.borrow().can_redo());
    let before = owner.state().unwrap();
    let permit = owner.capture(1, &current).unwrap();
    let answer =
        owner
            .apply_replacements_observed(
                &permit,
                std::iter::empty::<
                    std::result::Result<LiteralReplacement, MarkdownDocumentBufferError>,
                >(),
                MarkdownTextSelection::caret(2),
                &current,
            )
            .unwrap();
    assert!(matches!(answer, NativeMarkdownPagedInputOutcome::Unchanged(state) if state == before));
    assert_eq!(owner.state().unwrap(), before);
    assert_eq!(text(&document), "原文");
    assert!(document.borrow().can_redo());
}

#[test]
fn marked_batch_rejects_without_touching_candidate_and_postcommit_revocation_is_not_rollback() {
    let (_file, document) = fixture("原文", 4096);
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let permitted = Cell::new(true);
    let current = |_| permitted.get();
    let permit = owner.capture(1, &current).unwrap();
    owner
        .set_marked_text(&permit, "中", NSRange::new(1, 0), unspecified(), &current)
        .unwrap();
    let before = owner.state().unwrap();
    let permit = owner.capture(1, &current).unwrap();
    assert!(matches!(
        owner.apply_replacements_observed(
            &permit,
            [literal(0..1, "x")],
            MarkdownTextSelection::caret(1),
            &current
        ),
        Err(NativeMarkdownPagedInputError::CompositionActive)
    ));
    assert_eq!(owner.state().unwrap(), before);
    assert_eq!(text(&document), "中原文");
    let permit = owner.capture(1, &current).unwrap();
    committed(owner.unmark(&permit, &current).unwrap());
    let permit = owner.capture(1, &current).unwrap();
    let commit = committed(
        owner
            .apply_replacements_observed(
                &permit,
                [literal(0..1, "替")],
                MarkdownTextSelection::caret(1),
                &current,
            )
            .unwrap(),
    );
    permitted.set(false);
    assert!(matches!(
        commit.check_current(&current),
        Err(NativeMarkdownPagedInputError::Revoked)
    ));
    assert_eq!(text(&document), "替原文");
    assert!(document.borrow().can_undo());
}

#[test]
fn batch_current_never_crosses_document_or_fold_borrow_and_updates_real_fold_anchors() {
    let (_file, document) = fixture("# H\nbody\nend", 4096);
    let owner = NativeMarkdownPagedInput::with_folds(
        Rc::downgrade(&document),
        vec![MarkdownFoldRegion {
            header_line: 0,
            hidden_utf16_range: 3..8,
        }],
    )
    .unwrap();
    let current = |_| {
        assert!(document.try_borrow_mut().is_ok());
        assert!(owner.folds.try_borrow_mut().is_ok());
        true
    };
    let permit = owner.capture(1, &current).unwrap();
    let commit = committed(
        owner
            .apply_replacements_observed(
                &permit,
                [literal(0..0, "🙂"), literal(10..11, "尾")],
                MarkdownTextSelection::caret(2),
                &current,
            )
            .unwrap(),
    );
    assert!(commit.invalidation.folds_changed);
    assert_eq!(
        &*owner.folds.borrow(),
        &[MarkdownFoldRegion {
            header_line: 0,
            hidden_utf16_range: 5..10
        }]
    );
    assert_eq!(text(&document), "🙂# H\nbody\ne尾d");
}
