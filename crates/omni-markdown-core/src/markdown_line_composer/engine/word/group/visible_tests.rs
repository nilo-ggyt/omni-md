//! Visible LTR words use actual grapheme/paragraph permission without reading distant folded RTL bodies.

use super::super::tests::fixture;
use super::*;

/// Prepare the same original composer without promoting a visible scope into whole-source zero level.
fn prepare_visible(composer: &mut MarkdownLineComposer, budget: MarkdownLineComposeBudget) {
    for _ in 0..100_000 {
        let step = composer.prepare_word_move(budget).unwrap();
        assert!(step.bytes <= budget.bytes && step.scalars <= budget.scalars);
        assert!(step.metadata_units <= budget.metadata_units);
        if let Some(scope) = step.scope {
            assert_eq!(scope, MarkdownWordMoveScope::VisibleParagraphScopes);
            return;
        }
    }
    panic!("实际折叠来源没有完成 Bidi 职责准备");
}

/// Preserve each actual work quota and return only the operation's genuine terminal answer.
fn finish_visible(
    composer: &mut MarkdownLineComposer,
    budget: MarkdownLineComposeBudget,
) -> (MarkdownWordGroupAnswer, usize) {
    let mut bytes = 0;
    for turn in 0..100_000 {
        let context = match composer.payload.as_ref().unwrap().word_delete.as_ref() {
            Some(WordJob::Group(group)) => Some((
                group.head,
                group.cursor.direction,
                group.cursor.at.map(|(_, boundary)| boundary.utf16),
                group
                    .cursor
                    .cluster
                    .as_ref()
                    .map(|cluster| cluster.utf16..cluster.end.utf16),
                group.cursor.visible_proof_next,
                group.restore_position,
                group.deferred_answer.is_some(),
            )),
            _ => None,
        }; // Save finite diagnostic coordinates before failure retires the real owner; retain no source payload.
        let step = composer.step_word_group(budget).unwrap_or_else(|error| {
            panic!("实际词组量子{turn}失败：{error:?}；head/direction/GCB/cluster/proof/restore/deferred={context:?}")
        });
        assert!(step.bytes <= budget.bytes && step.scalars <= budget.scalars);
        assert!(step.records <= budget.records && step.metadata_units <= budget.metadata_units);
        bytes += step.bytes;
        if !matches!(step.answer, MarkdownWordGroupAnswer::Yielded) {
            return (step.answer, bytes);
        }
    }
    panic!("原可见词组游标没有到达真实终点");
}

#[test]
/// Move through actual visible CRLF paragraphs and request fold expansion without scanning its large body.
fn visible_ltr_word_uses_real_paragraphs_and_never_reads_the_hidden_rtl_body() {
    // The real source contains a large hidden RTL body. Only its original folded
    // range is installed; visual queries remain bound to actual visible scopes.
    let hidden = "אב😀\r\n".repeat(65_536);
    let source = format!("one two\r\n\r\n# H\r\n{hidden}# N\r\nlast word\r\n");
    let start = "one two\r\n\r\n# H".len();
    let end = "one two\r\n\r\n# H\r\n".len() + hidden.encode_utf16().count();
    let (_directory, document, mut composer) = fixture(&source);
    composer
        .install_folded_ranges(std::slice::from_ref(&(start..end)))
        .unwrap();
    let resources = composer.worker_resource_observer();
    let budget = MarkdownLineComposeBudget {
        bytes: 1,
        scalars: 1,
        records: 1,
        metadata_units: 1,
    };
    prepare_visible(&mut composer, budget);
    let mut read = 0;
    // Both directions and crossing physical empty paragraphs retain the original
    // action semantics while each real quantum is charged to the visible work.
    for (direction, head, expected) in [
        (MarkdownWordDeleteDirection::Forward, 0, 0..3),
        (MarkdownWordDeleteDirection::Backward, 7, 4..7),
        (MarkdownWordDeleteDirection::Forward, 7, 7..12),
    ] {
        composer.begin_visual_word_group(direction, head).unwrap();
        let (answer, bytes) = finish_visible(&mut composer, budget);
        read += bytes;
        assert_eq!(answer, MarkdownWordGroupAnswer::Ready(expected));
        composer.cancel_word_group();
    }
    // Reaching the opaque original boundary must return its exact expansion
    // need, never a visual target obtained by reading or skipping hidden text.
    composer
        .begin_visual_word_group(MarkdownWordDeleteDirection::Forward, start)
        .unwrap();
    let (answer, bytes) = finish_visible(&mut composer, budget);
    read += bytes;
    assert_eq!(
        answer,
        MarkdownWordGroupAnswer::NeedsFoldExpansion(start..end)
    );
    assert!(
        read < 512,
        "这里只能读取可见真实 B 段与 GCB，不能扫描巨型 RTL 隐区"
    );
    assert_eq!(document.generation(), 0);
    assert!(!document.can_undo() && !document.can_redo());
    drop((composer, document));
    assert!(resources.is_released());
}

#[test]
/// A nonzero-level visible paragraph refuses visual substitution while its logical selection stays healthy.
fn visible_rtl_word_is_an_explicit_capability_result_instead_of_a_false_logical_target() {
    // The visible paragraph itself contains RTL. A folded later paragraph cannot
    // authorize replacing this visual request with a merely logical endpoint.
    let source = "one אבג\r\n# H\r\n隐藏\r\n# N\r\n";
    let start = "one אבג\r\n# H".encode_utf16().count();
    let end = "one אבג\r\n# H\r\n隐藏\r\n".encode_utf16().count();
    let (_directory, document, mut composer) = fixture(source);
    composer
        .install_folded_ranges(std::slice::from_ref(&(start..end)))
        .unwrap();
    let resources = composer.worker_resource_observer();
    let budget = MarkdownLineComposeBudget {
        bytes: 3,
        scalars: 1,
        records: 1,
        metadata_units: 1,
    };
    prepare_visible(&mut composer, budget);
    composer
        .begin_visual_word_group(MarkdownWordDeleteDirection::Forward, 0)
        .unwrap();
    let (answer, _) = finish_visible(&mut composer, budget);
    assert_eq!(answer, MarkdownWordGroupAnswer::UnsupportedVisualOrder);
    // Capability refusal is recoverable: cancel only that action, then obtain
    // the actual logical selection on the same healthy original source.
    composer.cancel_word_group();
    composer
        .begin_word_group(
            MarkdownWordGroupAction::Select {
                backward_bias: false,
            },
            1,
        )
        .unwrap();
    let (answer, _) = finish_visible(&mut composer, budget);
    assert_eq!(answer, MarkdownWordGroupAnswer::Ready(0..3));
    assert_eq!(document.generation(), 0);
    drop((composer, document));
    assert!(resources.is_released());
}

#[test]
/// Cancel after the real scoped cursor has an endpoint, retaining an earlier reclaim failure when present.
fn visible_word_preserves_source_reclaim_failure_before_cancellation() {
    // Keep the genuine pending scope alive until it has read a real endpoint;
    // cancellation before cursor creation would not exercise its resource lifetime.
    for fail_source in [false, true] {
        let source = "one two\r\n# H\r\nאבג\r\n# N\r\n";
        let start = "one two\r\n# H".len();
        let end = "one two\r\n# H\r\nאבג\r\n".encode_utf16().count();
        let (_directory, document, mut composer) = fixture(source);
        composer
            .install_folded_ranges(std::slice::from_ref(&(start..end)))
            .unwrap();
        let resources = composer.worker_resource_observer();
        let budget = MarkdownLineComposeBudget {
            bytes: 1,
            scalars: 1,
            records: 1,
            metadata_units: 1,
        };
        prepare_visible(&mut composer, budget);
        composer
            .begin_visual_word_group(MarkdownWordDeleteDirection::Forward, 0)
            .unwrap();
        for _ in 0..100_000 {
            let _ = composer.step_word_group(budget).unwrap();
            let Some(WordJob::Group(group)) = &composer.payload.as_ref().unwrap().word_delete
            else {
                panic!("原 scoped 游标已丢失")
            };
            if group.cursor.cluster.is_some() {
                break;
            }
        }
        let Some(WordJob::Group(group)) = &composer.payload.as_ref().unwrap().word_delete else {
            unreachable!()
        };
        assert!(
            group.cursor.cluster.is_some(),
            "取消必须发生在实际端点已读到的原 scoped job"
        );
        if fail_source {
            // Consume the existing EIO injection through actual resident backing
            // reclaim. The test does not claim a naturally occurring kernel error.
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
        // Source failure precedes the later cancellation, while both terminal
        // paths must retire the real worker resources after their strong owners drop.
        composer.cancellation.cancel();
        let error = composer.step_word_group(budget);
        if fail_source {
            assert!(matches!(
                error,
                Err(MarkdownLineComposeError::Source(
                    MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
                ))
            ));
        } else {
            assert!(matches!(error, Err(MarkdownLineComposeError::Cancelled)));
        }
        drop((composer, document));
        assert!(resources.snapshot().unwrap().actual_resources_released());
    }
}
