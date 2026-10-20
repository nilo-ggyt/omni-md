//! 可见 LTR 词组逐实际 GCB/B 段授权，远处隐藏 RTL 不能迫使它拒绝或读取整个隐区。

use super::super::tests::fixture;
use super::*;

/// 只驱动同一 Composer 的原准备量子；分支结果不从任何子段补签为整源零级。
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

/// 每步仍核原 4KiB/标量/记录量子；只返回真实终点，未决定的范围不能被测试推成 Ready。
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
        }; // 原 finish 会在真实失败时退役 owner；先保有限坐标供定位，不保活 Source 或段结果。
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
/// 同一请求前后移动、跨真实 CRLF 段继续消耗及 opaque 需求均沿原 owner；巨隐区字节不被读入。
fn visible_ltr_word_uses_real_paragraphs_and_never_reads_the_hidden_rtl_body() {
    let hidden = "אב😀\r\n".repeat(65_536);
    let source = format!("one two\r\n\r\n# H\r\n{hidden}# N\r\nlast word\r\n");
    let start = "one two\r\n\r\n# H".len();
    let end = "one two\r\n\r\n# H\r\n".len() + hidden.encode_utf16().count();
    let (_directory, document, mut composer) = fixture(&source);
    composer
        .install_folded_ranges(std::slice::from_ref(&(start..end)))
        .unwrap();
    let resources = composer.worker_resource_observer();
    let original = std::ptr::from_ref(&composer.payload.as_ref().unwrap().coordinator);
    let budget = MarkdownLineComposeBudget {
        bytes: 1,
        scalars: 1,
        records: 1,
        metadata_units: 1,
    };
    prepare_visible(&mut composer, budget);
    let mut read = 0;
    for (direction, head, expected) in [
        (MarkdownWordDeleteDirection::Forward, 0, 0..3),
        (MarkdownWordDeleteDirection::Backward, 7, 4..7),
        (MarkdownWordDeleteDirection::Forward, 7, 7..12),
    ] {
        composer.begin_visual_word_group(direction, head).unwrap();
        let (answer, bytes) = finish_visible(&mut composer, budget);
        read += bytes;
        assert_eq!(answer, MarkdownWordGroupAnswer::Ready(expected));
        // 第三个真实 B 段会换掉两槽中的原布局段；恢复后只查询其原元数据，不凭空复活已退休的 proof。
        let mut restored = false;
        for _ in 0..100 {
            let (step, proof) = composer
                .payload
                .as_mut()
                .unwrap()
                .coordinator
                .prepare_word_cluster(0..1, budget)
                .unwrap();
            assert_eq!((step.bytes, step.scalars), (0, 0));
            assert!(step.records <= budget.records && step.metadata_units <= budget.metadata_units);
            if let Some((proof, through)) = proof {
                assert!(proof);
                assert_eq!(through, 1);
                restored = true;
                break;
            }
        }
        assert!(
            restored,
            "原布局段必须已恢复，真实元数据查询必须在原记录量子内完成"
        );
        composer.cancel_word_group();
    }
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
    assert_eq!(
        std::ptr::from_ref(&composer.payload.as_ref().unwrap().coordinator),
        original
    );
    assert_eq!(document.generation(), 0);
    assert!(!document.can_undo() && !document.can_redo());
    drop((composer, document));
    assert!(resources.is_released());
}

#[test]
/// 非零级可见 B 段仍明确拒绝视觉逻辑替代；拒绝后同源逻辑选词健康，真实 UBA 文件随 owner 释放。
fn visible_rtl_word_is_an_explicit_capability_result_instead_of_a_false_logical_target() {
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
/// 在真实 scoped 游标已有端点后取消；实际 backing 回收 EIO 仍先于取消，全部原资源最终归还。
fn visible_word_preserves_source_reclaim_failure_before_cancellation() {
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
