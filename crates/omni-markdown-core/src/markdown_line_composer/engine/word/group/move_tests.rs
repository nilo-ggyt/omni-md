//! Visual word movement requires actual whole-source bidi permission and never edits text or history.

use super::super::tests::{ascii_fixture, fixture};
use super::tests::drive;
use super::*;

fn prepare(composer: &mut MarkdownLineComposer, budget: MarkdownLineComposeBudget) -> bool {
    // Only a completed genuine source scope can authorize visual movement.
    // Unsupported is a capability result, not a failed source or fabricated EOF.
    let maximum = composer.payload.as_ref().unwrap().source.byte_len() * 100 + 512;
    for _ in 0..maximum {
        let step = composer.prepare_word_move(budget).unwrap();
        assert!(step.bytes <= budget.bytes.min(4096));
        assert!(step.scalars <= budget.scalars.min(256));
        assert!(step.metadata_units <= budget.metadata_units.min(256));
        if let Some(scope) = step.scope {
            return scope == MarkdownWordMoveScope::WholeUniformZero;
        }
    }
    panic!("同源原Bidi没有完成视觉Word能力准备");
}

#[test]
fn visual_word_requires_complete_source_uniform_zero_and_preserves_logical_groups() {
    // Whole-source zero-level permission makes visual LTR movement agree with
    // these original logical ranges, including hard lines and complete graphemes.
    for (text, head, backward, forward) in [
        ("hello  world", 5, 0, 12),
        ("one\t\r\n\r\ntwo", 3, 0, 11),
        ("中文分组 测试", 2, 0, 4),
        ("e\u{301}clair !", 2, 0, 7),
        ("👩‍👩‍👧‍👦😀!", 0, 0, 14),
    ] {
        let (_directory, document, mut composer) = fixture(text);
        let budget = MarkdownLineComposeBudget {
            bytes: 1,
            scalars: 1,
            records: 1,
            metadata_units: 1,
        };
        assert!(prepare(&mut composer, budget));
        // Reuse the original prepared owner for both directions while retiring
        // each finished action; no direction query may mutate document history.
        for (action, expected) in [
            (MarkdownWordGroupAction::Backward, backward..head),
            (MarkdownWordGroupAction::Forward, head..forward),
        ] {
            composer.begin_word_group(action, head).unwrap();
            assert_eq!(drive(&mut composer, budget), expected);
            composer.cancel_word_group();
        }
        assert_eq!(document.generation(), 0);
        assert!(!document.can_undo() && !document.can_redo());
    }
}

#[test]
fn late_rtl_arabic_number_and_isolate_are_not_visual_permission_or_source_failure() {
    // A late nonzero bidi level or control invalidates whole-source zero-level
    // capability even when the visible prefix looks LTR. The source remains healthy.
    for text in [
        "one two אבג",
        "one two \u{661}",
        "one \u{2067}two\u{2069}",
        "one \u{202b}two\u{202c}",
    ] {
        let (_directory, document, mut composer) = fixture(text);
        let resources = composer.worker_resource_observer();
        assert!(!prepare(
            &mut composer,
            MarkdownLineComposeBudget::default()
        ));
        assert!(!prepare(
            &mut composer,
            MarkdownLineComposeBudget::default()
        ));
        composer
            .begin_word_group(
                MarkdownWordGroupAction::Select {
                    backward_bias: false,
                },
                1,
            )
            .unwrap();
        assert_eq!(
            drive(&mut composer, MarkdownLineComposeBudget::default()),
            0..3
        );
        composer.cancel_word_group(); // Capability refusal does not poison real logical selection or deletion.
        composer
            .begin_word_delete(MarkdownWordDeleteDirection::Forward, 0)
            .unwrap();
        assert_eq!(
            super::super::tests::drive(&mut composer, MarkdownLineComposeBudget::default()),
            0..3
        );
        assert_eq!(document.generation(), 0);
        drop((composer, document));
        assert!(resources.is_released());
    }
}

#[test]
fn four_mebibyte_word_move_reuses_original_root_proof_and_bounded_gcb() {
    // Reuse the original tree's full-source permission and bounded GCB work; a
    // correct endpoint must not be obtained by silently reading the entire long word.
    let length = 4 * 1024 * 1024;
    let (_directory, document, mut composer) = ascii_fixture(length);
    let resources = composer.worker_resource_observer();
    let mapping = document.original_backing_observer().unwrap();
    let mut bytes = 0;
    for _ in 0..128 {
        let step = composer
            .prepare_word_move(MarkdownLineComposeBudget::default())
            .unwrap();
        bytes += step.bytes;
        assert!(step.bytes <= 4096 && step.scalars <= 256 && step.metadata_units <= 256);
        if step.scope == Some(MarkdownWordMoveScope::WholeUniformZero) {
            break;
        }
        assert_ne!(step.scope, Some(MarkdownWordMoveScope::Unsupported));
    }
    assert!(prepare(&mut composer, MarkdownLineComposeBudget::default()));
    // After genuine capability preparation, move through the same source and
    // aggregate actual reported bytes across preparation and word traversal.
    composer
        .begin_word_group(MarkdownWordGroupAction::Forward, 7)
        .unwrap();
    let mut moved = None;
    for _ in 0..128 {
        let step = composer
            .step_word_group(MarkdownLineComposeBudget::default())
            .unwrap();
        bytes += step.bytes;
        if let MarkdownWordGroupAnswer::Ready(range) = step.answer {
            moved = Some(range);
            break;
        }
    }
    assert_eq!(moved, Some(7..7 + length));
    assert!(bytes < 64 * 1024, "移动不得隐式读取原四MiB整词");
    assert_eq!(document.generation(), 0);
    drop((composer, document));
    assert!(resources.is_released());
    assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0);
}

#[test]
fn word_move_cancel_and_original_eio_keep_original_cause_and_release() {
    // Begin actual capability preparation before revocation. The two cases
    // distinguish ordinary cancellation from an earlier original-source failure.
    for failed_io in [false, true] {
        let (_directory, document, mut composer) = fixture("one 中文");
        let resources = composer.worker_resource_observer();
        let mapping = document.original_backing_observer().unwrap();
        composer
            .prepare_word_move(MarkdownLineComposeBudget {
                bytes: 1,
                scalars: 1,
                records: 1,
                metadata_units: 1,
            })
            .unwrap();
        if failed_io {
            // Pin a real original byte and consume the existing EIO injection
            // through reclaim; this is not evidence of a natural kernel fault.
            document
                .text_snapshot()
                .visit_byte_range(0..1, |_| Ok(()))
                .unwrap();
            document.fail_next_original_reclaim_for_test();
            assert_eq!(
                document.reclaim_original_read_windows_for_test(),
                Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                    libc::EIO
                ))
            );
        }
        // Later cancellation cannot replace the first source cause, and both
        // paths must release the actual worker and original mapping on retirement.
        composer.cancellation.cancel();
        let result = composer.prepare_word_move(MarkdownLineComposeBudget::default());
        if failed_io {
            assert!(matches!(
                result,
                Err(MarkdownLineComposeError::Source(
                    MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
                ))
            ));
        } else {
            assert!(matches!(result, Err(MarkdownLineComposeError::Cancelled)));
        }
        drop((composer, document));
        assert!(resources.snapshot().unwrap().actual_resources_released());
        assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0);
    }
}
