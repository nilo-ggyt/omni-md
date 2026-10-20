//! 固定版本CodeMirror实际有限输出只作oracle；产品仍消费原Source/GCB，不执行脚本。

use super::super::tests::{ascii_fixture, fixture};
use super::*;

pub(super) fn drive(
    composer: &mut MarkdownLineComposer,
    budget: MarkdownLineComposeBudget,
) -> Range<usize> {
    let maximum = composer.payload.as_ref().unwrap().source.byte_len() * 16 + 512;
    for _ in 0..maximum {
        let step = composer.step_word_group(budget).unwrap();
        assert!(step.bytes <= budget.bytes.min(4096));
        assert!(step.scalars <= budget.scalars.min(256));
        assert!(step.records <= budget.records.min(256));
        assert!(step.metadata_units <= budget.metadata_units.min(256));
        match step.answer {
            MarkdownWordGroupAnswer::Ready(range) => return range,
            MarkdownWordGroupAnswer::Yielded => {}
            MarkdownWordGroupAnswer::NeedsFoldExpansion(_) => {
                panic!("无 fold 的原逻辑 oracle 不得要求展开")
            }
            MarkdownWordGroupAnswer::UnsupportedVisualOrder => {
                panic!("原逻辑 oracle 不请求视觉证书")
            }
        }
    }
    panic!("原有界词组没有到达真实边界");
}

#[test]
fn logical_ltr_groups_and_hard_line_selection_match_actual_codemirror_6_11_oracle() {
    // 2026-10-19直接执行Host中commands6.11.0/view6.43.9/state6.7.1的moveByGroup与groupAt。
    // oracle保持真实LTR视觉序等于逻辑序；混合Bidi没有被这些数值授予视觉移动语义。
    for (text, head, backward, forward, left, right) in [
        ("hello  world", 5, 0, 12, 0..5, 5..7),
        ("hello\t\n\nworld", 5, 0, 13, 0..5, 5..6),
        ("foo!!!bar", 3, 0, 6, 0..3, 3..6),
        ("a_12!", 0, 0, 4, 0..4, 0..4),
        ("中文分组 测试", 2, 0, 4, 0..4, 0..4),
        ("e\u{301}clair !", 2, 0, 7, 0..7, 0..7),
        ("👩‍👩‍👧‍👦😀!", 0, 0, 14, 0..14, 0..14),
        ("first\nnext", 5, 0, 10, 0..5, 0..5),
        ("first\nnext", 6, 0, 10, 6..10, 6..10),
        ("first\n\nnext", 6, 0, 11, 6..6, 6..6),
        ("a\u{feff} b", 1, 0, 4, 0..1, 1..3),
        ("a\u{85}!b", 1, 0, 3, 0..1, 1..3),
        ("foo.bar", 3, 0, 4, 0..3, 3..4),
        ("foo.bar", 4, 3, 7, 3..4, 4..7),
        ("", 0, 0, 0, 0..0, 0..0),
        (" x ", 0, 0, 2, 0..1, 0..1),
        (" x ", 3, 1, 3, 2..3, 2..3),
        ("one\n\n", 5, 0, 5, 5..5, 5..5),
    ] {
        let (_directory, document, mut composer) = fixture(text);
        for (action, expected) in [
            (MarkdownWordGroupAction::Backward, backward..head),
            (MarkdownWordGroupAction::Forward, head..forward),
            (
                MarkdownWordGroupAction::Select {
                    backward_bias: true,
                },
                left,
            ),
            (
                MarkdownWordGroupAction::Select {
                    backward_bias: false,
                },
                right,
            ),
        ] {
            composer.begin_word_group(action, head).unwrap();
            let budget = MarkdownLineComposeBudget {
                bytes: 1,
                scalars: 1,
                records: 1,
                metadata_units: 1,
            };
            assert_eq!(
                drive(&mut composer, budget),
                expected,
                "{text:?}, {head}, {action:?}"
            );
            assert_eq!(drive(&mut composer, budget), expected); // Ready仍核同一原Source/GCB，不以旧范围绕过健康门。
            composer.cancel_word_group();
        }
        assert_eq!(document.generation(), 0);
        assert!(!document.is_dirty() && !document.can_undo() && !document.can_redo());
    }
}

#[test]
fn select_keeps_physical_crlf_and_logical_mixed_bidi_source_order() {
    for (text, head, backward_bias, expected) in [
        ("first\r\nnext", 5, false, 0..5),
        ("first\r\nnext", 7, true, 7..11),
        ("first\r\n\r\nnext", 7, true, 7..7),
        ("a אבג !", 3, false, 2..5),
        ("a אבג !", 3, true, 2..5),
    ] {
        let (_directory, document, mut composer) = fixture(text);
        composer
            .begin_word_group(MarkdownWordGroupAction::Select { backward_bias }, head)
            .unwrap();
        assert_eq!(
            drive(&mut composer, MarkdownLineComposeBudget::default()),
            expected
        );
        assert_eq!(document.generation(), 0);
        assert!(!document.can_undo());
    } // Bidi选词只扩实际逻辑相邻类别；没有用它冒充视觉Left/Right移动。
}

#[test]
fn original_multi_mebibyte_word_selection_is_bounded_and_releases_the_real_source() {
    for length in [2 * 1024 * 1024, 4 * 1024 * 1024] {
        let (_directory, document, mut composer) = ascii_fixture(length);
        let resources = composer.worker_resource_observer();
        let backing = document.original_backing_observer().unwrap();
        composer
            .begin_word_group(
                MarkdownWordGroupAction::Select {
                    backward_bias: false,
                },
                7 + length / 2,
            )
            .unwrap();
        let mut ready = None;
        for _ in 0..128 {
            let step = composer
                .step_word_group(MarkdownLineComposeBudget::default())
                .unwrap();
            assert!(
                step.bytes <= 4096
                    && step.scalars <= 256
                    && step.records <= 256
                    && step.metadata_units <= 256
            );
            if let MarkdownWordGroupAnswer::Ready(range) = step.answer {
                ready = Some(range);
                break;
            }
        }
        assert_eq!(ready, Some(7..7 + length));
        assert!(!document.can_undo() && document.generation() == 0);
        drop((composer, document));
        assert!(resources.is_released());
        assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0);
    }
}

#[test]
fn group_cancellation_and_original_eio_do_not_leak_or_reuse_a_ready_range() {
    let (_directory, document, mut composer) = fixture("中文 选择 👩‍👩‍👧‍👦!");
    let backing = document.original_backing_observer().unwrap();
    let resources = composer.worker_resource_observer();
    composer
        .begin_word_group(
            MarkdownWordGroupAction::Select {
                backward_bias: false,
            },
            4,
        )
        .unwrap();
    composer
        .step_word_group(MarkdownLineComposeBudget {
            bytes: 1,
            scalars: 1,
            records: 1,
            metadata_units: 1,
        })
        .unwrap();
    composer.cancel_word_group();
    composer
        .begin_word_delete(MarkdownWordDeleteDirection::Forward, 0)
        .unwrap();
    assert_eq!(
        super::super::tests::drive(&mut composer, MarkdownLineComposeBudget::default()),
        0..2
    );
    composer.cancel_word_delete();
    composer
        .begin_word_group(
            MarkdownWordGroupAction::Select {
                backward_bias: false,
            },
            0,
        )
        .unwrap();
    assert_eq!(
        drive(&mut composer, MarkdownLineComposeBudget::default()),
        0..2
    );
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
    composer.cancellation.cancel();
    assert!(matches!(
        composer.step_word_group(MarkdownLineComposeBudget::default()),
        Err(MarkdownLineComposeError::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
    drop((composer, document));
    let facts = resources.snapshot().unwrap();
    assert_eq!(facts.first_source_errno, Some(libc::EIO));
    assert!(facts.actual_resources_released());
    assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0);
}

#[test]
fn a_mid_grapheme_group_request_fails_without_publishing_a_partial_selection() {
    let (_directory, document, mut composer) = fixture("xe\u{301}😀y");
    composer
        .begin_word_group(
            MarkdownWordGroupAction::Select {
                backward_bias: false,
            },
            2,
        )
        .unwrap();
    let mut error = None;
    for _ in 0..64 {
        match composer.step_word_group(MarkdownLineComposeBudget::default()) {
            Ok(step) => assert!(matches!(step.answer, MarkdownWordGroupAnswer::Yielded)),
            Err(actual) => {
                error = Some(actual);
                break;
            }
        }
    }
    assert!(matches!(
        error,
        Some(MarkdownLineComposeError::InvalidRequest)
    ));
    assert!(!document.can_undo() && document.generation() == 0);
}
