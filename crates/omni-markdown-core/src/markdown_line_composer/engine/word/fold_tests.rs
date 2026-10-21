//! Opaque folds yield recoverable expansion needs while retaining actual work and source ownership.

use super::*;

#[test]
// Adjacent visible endpoints around a large fold are not one original grapheme.
// The recoverable expansion result must leave the original composer usable.
fn opaque_grapheme_delete_keeps_real_work_and_the_original_healthy_owner() {
    // Install a genuine original folded range containing RTL, emoji and physical
    // CRLF. The delete request starts at its visible boundary, not inside a fake source.
    let body = "אב🙂\r\n".repeat(65_536);
    let text = format!("# H\r\n{body}next");
    let end = 5 + body.encode_utf16().count();
    let (_directory, document, mut composer) = tests::fixture(&text);
    composer
        .install_folded_ranges(std::slice::from_ref(&(3..end)))
        .unwrap();
    let resources = composer.worker_resource_observer();
    composer
        .begin_grapheme_delete(MarkdownWordDeleteDirection::Forward, 3)
        .unwrap();
    let budget = MarkdownLineComposeBudget {
        bytes: 1,
        scalars: 1,
        records: 1,
        metadata_units: 1,
    };
    let mut actual_bytes = 0usize;
    let mut found = false;
    // Continue with unit quotas until the real opaque boundary is encountered;
    // publishing a Ready deletion across hidden text would lose original GCB semantics.
    for _ in 0..100_000 {
        let step = composer.step_word_delete(budget).unwrap();
        assert!(
            step.bytes <= 1 && step.scalars <= 1 && step.records <= 1 && step.metadata_units <= 1
        );
        actual_bytes += step.bytes;
        match step.answer {
            MarkdownWordDeleteAnswer::Yielded => {}
            MarkdownWordDeleteAnswer::NeedsFoldExpansion(hidden) => {
                assert_eq!(hidden, 3..end);
                assert_eq!(step.records, 1);
                found = true;
                break;
            }
            MarkdownWordDeleteAnswer::Ready(_) => panic!("原隐区不能作为一字素删除范围发布"),
        }
    }
    assert!(found && actual_bytes < 64);
    // Return the pending action, then prove the same original worker can process
    // a visible grapheme without editing bytes or adding an undo operation.
    composer.cancel_word_delete();
    composer
        .begin_grapheme_delete(MarkdownWordDeleteDirection::Forward, 0)
        .unwrap();
    assert_eq!(tests::drive(&mut composer, budget), 0..1);
    assert_eq!(document.generation(), 0);
    assert!(!document.can_undo() && !document.can_redo());
    drop((composer, document));
    assert!(resources.is_released());
}

#[test]
// A genuine zero-level root permits LTR word movement, while an opaque fold
// still requests expansion and retains the original cancellation/resource graph.
fn opaque_word_move_retains_true_root_permission_and_preserves_cancellation() {
    // Prepare actual whole-source direction permission before approaching the fold;
    // fold expansion must not degrade that permission into a false capability error.
    let (_directory, document, mut composer) = tests::fixture("# H\nbody word\nnext");
    let end = "# H\nbody word\n".len();
    composer
        .install_folded_ranges(std::slice::from_ref(&(3..end)))
        .unwrap();
    let resources = composer.worker_resource_observer();
    let budget = MarkdownLineComposeBudget {
        bytes: 1,
        scalars: 1,
        records: 1,
        metadata_units: 1,
    };
    let prepared = composer.prepare_word_move(budget).unwrap();
    assert_eq!(
        prepared.scope,
        Some(MarkdownWordMoveScope::WholeUniformZero)
    );
    assert_eq!((prepared.bytes, prepared.scalars), (0, 0));
    composer
        .begin_word_group(MarkdownWordGroupAction::Forward, 3)
        .unwrap();
    let mut found = false;
    // Yield until the original folded boundary is reached. The original record
    // work remains charged even though the recoverable action cannot yet move.
    for _ in 0..100_000 {
        let step = composer.step_word_group(budget).unwrap();
        assert!(
            step.bytes <= 1 && step.scalars <= 1 && step.records <= 1 && step.metadata_units <= 1
        );
        match step.answer {
            MarkdownWordGroupAnswer::Yielded => {}
            MarkdownWordGroupAnswer::NeedsFoldExpansion(hidden) => {
                assert_eq!(hidden, 3..end);
                assert_eq!(step.records, 1);
                found = true;
                break;
            }
            MarkdownWordGroupAnswer::Ready(_) => panic!("隐藏文本未展开就不能签发后继词位置"),
            MarkdownWordGroupAnswer::UnsupportedVisualOrder => {
                panic!("真实 Root 零级证书不得退化为方向拒绝")
            }
        }
    }
    assert!(found);
    // Global cancellation after the expansion result must terminate the actual
    // pending work and release all resources after the remaining owners drop.
    composer.cancellation.cancel();
    assert!(matches!(
        composer.step_word_group(budget),
        Err(MarkdownLineComposeError::Cancelled)
    ));
    assert_eq!(document.generation(), 0);
    assert!(!document.can_undo() && !document.can_redo());
    drop((composer, document));
    assert!(resources.is_released());
}
