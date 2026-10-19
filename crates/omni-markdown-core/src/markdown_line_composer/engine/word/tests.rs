//! 合成原文件直接验证词组范围、一次编辑历史和真实源失败；oracle不进入产品状态。

use super::*;
use crate::{MarkdownDocumentBuffer, MarkdownTextSelection};

/// 原删除负控只观察确实属于删除的游标，不能把后来的选词意图当作同一个动作。
fn pending_delete(composer: &MarkdownLineComposer) -> &WordDelete {
    let Some(WordJob::Delete(word)) = composer.payload.as_ref().unwrap().word_delete.as_ref()
    else {
        panic!("原删除用例没有真实删除游标");
    };
    word
}

pub(super) fn fixture(
    text: &str,
) -> (
    tempfile::TempDir,
    MarkdownDocumentBuffer,
    MarkdownLineComposer,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("词组编辑.md");
    std::fs::write(&path, text).unwrap();
    let document = MarkdownDocumentBuffer::open(&path, text.len() as u64 + 65_536).unwrap();
    let source = MarkdownLayoutTextSource::capture(&document);
    let composer = MarkdownLineComposer::new(
        source.clone(),
        &source.authority(),
        MarkdownLineFontCatalog::new(),
        640 * 64,
        MarkdownLineComposeCancellation::new(),
    )
    .unwrap();
    (directory, document, composer)
}

pub(super) fn drive(
    composer: &mut MarkdownLineComposer,
    budget: MarkdownLineComposeBudget,
) -> Range<usize> {
    // 上限按原合成字节数覆盖GCB生成、二分和分类；不靠放宽产品deadline或缩小实际负载。
    let maximum = composer.payload.as_ref().unwrap().source.byte_len() * 8 + 256;
    for _ in 0..maximum {
        let step = composer.step_word_delete(budget).unwrap();
        assert!(step.bytes <= budget.bytes.min(4096));
        assert!(step.scalars <= budget.scalars.min(256));
        assert!(step.records <= budget.records.min(256));
        assert!(step.metadata_units <= budget.metadata_units.min(256));
        if let MarkdownWordDeleteAnswer::Ready(range) = step.answer {
            return range;
        }
    }
    panic!("有限原词组没有到达真实分类或行边界");
}

/// 大词按4KiB写入真实原文件；测试也不先建立两/四MiB的完整String期望副本。
pub(super) fn ascii_fixture(
    length: usize,
) -> (
    tempfile::TempDir,
    MarkdownDocumentBuffer,
    MarkdownLineComposer,
) {
    use std::io::Write;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("原始ASCII长词.md");
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(b"prefix ").unwrap();
    let block = [b'x'; crate::MARKDOWN_LAYOUT_SOURCE_BYTES];
    for _ in 0..length / block.len() {
        file.write_all(&block).unwrap();
    }
    file.write_all(&block[..length % block.len()]).unwrap();
    file.write_all(b"!").unwrap();
    drop(file); // 原文件写者先真实归还；后续只消费同一个冻结Source与原树摘要。
    let document = MarkdownDocumentBuffer::open(&path, (length + 65_536) as u64).unwrap();
    let source = MarkdownLayoutTextSource::capture(&document);
    let composer = MarkdownLineComposer::new(
        source.clone(),
        &source.authority(),
        MarkdownLineFontCatalog::new(),
        640 * 64,
        MarkdownLineComposeCancellation::new(),
    )
    .unwrap();
    (directory, document, composer)
}

#[test]
fn original_ascii_two_and_four_mebibyte_alnum_words_skip_interior_nodes_with_real_budgets() {
    for length in [2 * 1024 * 1024, 4 * 1024 * 1024] {
        for direction in [
            MarkdownWordDeleteDirection::Backward,
            MarkdownWordDeleteDirection::Forward,
        ] {
            let (_directory, document, mut composer) = ascii_fixture(length);
            let mapping = document.original_backing_observer().unwrap();
            let resources = composer.worker_resource_observer();
            let head = if direction == MarkdownWordDeleteDirection::Backward {
                7 + length
            } else {
                7
            };
            composer.begin_word_delete(direction, head).unwrap();
            let mut ready = None;
            let (mut bytes, mut scalars, mut metadata) = (0, 0, 0);
            for turn in 0..64 {
                let step = composer
                    .step_word_delete(MarkdownLineComposeBudget {
                        bytes: 4096,
                        scalars: 256,
                        records: 256,
                        metadata_units: 256,
                    })
                    .unwrap();
                assert!(
                    step.bytes <= 4096
                        && step.scalars <= 256
                        && step.records <= 256
                        && step.metadata_units <= 256
                );
                bytes += step.bytes;
                scalars += step.scalars;
                metadata += step.metadata_units;
                if let MarkdownWordDeleteAnswer::Ready(range) = step.answer {
                    ready = Some(range);
                    println!(
                        "word_ascii original_bytes={length} direction={direction:?} actual_turns={} actual_source_bytes={bytes} actual_scalars={scalars} actual_metadata={metadata}",
                        turn + 1
                    );
                    break;
                }
            }
            assert_eq!(ready, Some(7..7 + length));
            assert!(
                bytes <= 2 * crate::MARKDOWN_LAYOUT_SOURCE_BYTES + 4,
                "同一长词只允许两个边叶与原停止字符读取"
            );
            assert!(scalars <= 2 * crate::MARKDOWN_LAYOUT_SOURCE_BYTES + 4);
            assert_eq!(document.generation(), 0);
            assert!(!document.can_undo() && !document.is_dirty());
            drop((composer, document));
            assert!(resources.is_released());
            assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0);
        } // 两方向都在原真实源上跳过中间节点；不拿一次范围计算冒充编辑/IME验收。
    }
}

#[test]
fn ascii_original_span_and_partial_leaf_progress_with_one_unit_budgets() {
    let length = 4 * 1024 * 1024;
    for direction in [
        MarkdownWordDeleteDirection::Backward,
        MarkdownWordDeleteDirection::Forward,
    ] {
        let (_directory, document, mut composer) = ascii_fixture(length);
        let head = if direction == MarkdownWordDeleteDirection::Backward {
            length + 7
        } else {
            7
        };
        composer.begin_word_delete(direction, head).unwrap();
        let mut ready = None;
        let mut bytes = 0;
        for _ in 0..2 * crate::MARKDOWN_LAYOUT_SOURCE_BYTES + 512 {
            let step = composer
                .step_word_delete(MarkdownLineComposeBudget {
                    bytes: 1,
                    scalars: 1,
                    records: 1,
                    metadata_units: 1,
                })
                .unwrap();
            assert!(
                step.bytes <= 1
                    && step.scalars <= 1
                    && step.records <= 1
                    && step.metadata_units <= 1
            );
            bytes += step.bytes;
            if let MarkdownWordDeleteAnswer::Ready(range) = step.answer {
                ready = Some(range);
                break;
            }
        }
        assert_eq!(ready, Some(7..7 + length)); // 反复从root/Span前缀重开将无法在此真实小预算内完成。
        assert!(bytes <= 2 * crate::MARKDOWN_LAYOUT_SOURCE_BYTES + 4);
        assert!(!document.can_undo());
    }
}

#[test]
fn ascii_skip_keeps_space_punctuation_underscore_and_non_ascii_gcb_rules() {
    for (text, head, direction, expected) in [
        ("a_b_c!", 5, MarkdownWordDeleteDirection::Backward, 0..5),
        ("a_b_c!", 0, MarkdownWordDeleteDirection::Forward, 0..5),
        ("a  b", 3, MarkdownWordDeleteDirection::Backward, 1..3),
        ("a b", 2, MarkdownWordDeleteDirection::Backward, 0..2),
        ("a!!b", 1, MarkdownWordDeleteDirection::Forward, 1..3),
        ("x\r\ny", 3, MarkdownWordDeleteDirection::Backward, 1..3),
        ("a\u{301}b", 2, MarkdownWordDeleteDirection::Backward, 0..2),
    ] {
        let (_directory, document, mut composer) = fixture(text);
        composer.begin_word_delete(direction, head).unwrap();
        assert_eq!(
            drive(
                &mut composer,
                MarkdownLineComposeBudget {
                    bytes: 1,
                    scalars: 1,
                    records: 1,
                    metadata_units: 1
                }
            ),
            expected
        );
        assert!(!document.can_undo() && !document.is_dirty());
    } // 可打印不等于Word；跨叶/范围外的标记或CRLF也不能由局部ASCII正事实切断。
}

#[test]
fn cancelling_a_live_ascii_tree_cursor_releases_the_original_worker_and_mapping() {
    let (_directory, document, mut composer) = ascii_fixture(4 * 1024 * 1024);
    let mapping = document.original_backing_observer().unwrap();
    let resources = composer.worker_resource_observer();
    composer
        .begin_word_delete(
            MarkdownWordDeleteDirection::Backward,
            document.utf16_len() - 1,
        )
        .unwrap();
    let mut entered = false;
    for _ in 0..256 {
        composer
            .step_word_delete(MarkdownLineComposeBudget {
                bytes: 1,
                scalars: 1,
                records: 1,
                metadata_units: 1,
            })
            .unwrap();
        if pending_delete(&composer).ascii_run.is_some() {
            entered = true;
            break;
        }
    }
    assert!(entered); // 必须确有持原节点Arc的在途游标，不能在尚未创建前假测取消。
    composer.cancel_word_delete();
    assert!(composer.payload.as_ref().unwrap().word_delete.is_none());
    drop((composer, document));
    assert!(resources.is_released());
    assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0);
}

#[test]
fn live_ascii_cursor_keeps_source_eio_before_cancellation_and_really_releases() {
    let (_directory, document, mut composer) = ascii_fixture(2 * 1024 * 1024);
    let mapping = document.original_backing_observer().unwrap();
    let resources = composer.worker_resource_observer();
    composer
        .begin_word_delete(
            MarkdownWordDeleteDirection::Backward,
            document.utf16_len() - 1,
        )
        .unwrap();
    for _ in 0..256 {
        composer
            .step_word_delete(MarkdownLineComposeBudget {
                bytes: 1,
                scalars: 1,
                records: 1,
                metadata_units: 1,
            })
            .unwrap();
        if pending_delete(&composer).ascii_run.is_some() {
            break;
        }
    }
    assert!(pending_delete(&composer).ascii_run.is_some());
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
        composer.step_word_delete(MarkdownLineComposeBudget::default()),
        Err(MarkdownLineComposeError::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
    drop((composer, document));
    let actual = resources.snapshot().unwrap();
    assert_eq!(actual.first_source_errno, Some(libc::EIO));
    assert!(actual.actual_resources_released());
    assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0);
}

#[test]
fn ascii_tree_cursor_rejects_another_capture_of_the_same_document() {
    let (_directory, document, composer) = fixture("same original ASCII word");
    let source = composer.payload.as_ref().unwrap().source.clone();
    let mut cursor = source.begin_ascii_word_run(0, true).unwrap();
    let mut baseline = source.begin_ascii_word_run(0, true).unwrap();
    let another = MarkdownLayoutTextSource::capture(&document);
    assert!(matches!(
        another.advance_ascii_word_run(&mut cursor, 1, 1, 1),
        Err(MarkdownLineComposeError::SourceMismatch)
    ));
    let mut endpoint = None;
    for _ in 0..document.utf16_len() + 8 {
        let actual = source.advance_ascii_word_run(&mut cursor, 1, 1, 1).unwrap();
        let expected = source
            .advance_ascii_word_run(&mut baseline, 1, 1, 1)
            .unwrap();
        assert_eq!(
            (
                actual.bytes,
                actual.scalars,
                actual.metadata,
                actual.position,
                actual.complete
            ),
            (
                expected.bytes,
                expected.scalars,
                expected.metadata,
                expected.position,
                expected.complete
            )
        ); // 外国capture不能消耗原游标的节点或字节续体；对照在拒绝前就属于同一原Source。
        if actual.complete {
            endpoint = Some(actual.position);
            break;
        } // metadata与字节独立收费；合法量子可先访问root，再用自己的byte预算读叶。
    }
    assert_eq!(endpoint, Some(4)); // 真实空格之前的same结束，不能只比较两条同样未完成的续体。
    assert_eq!(document.generation(), 0);
    assert!(!document.can_undo()); // 拒绝后原游标未被其它capture推进，原文稿与历史也未改变。
}

#[test]
fn actual_gcb_groups_keep_chinese_emoji_combining_rtl_and_space_deletion_rules() {
    let tiny = MarkdownLineComposeBudget {
        bytes: 1,
        scalars: 1,
        records: 1,
        metadata_units: 1,
    };
    for (text, head, direction, expected) in [
        (
            "hello world",
            6,
            MarkdownWordDeleteDirection::Backward,
            0..6,
        ),
        (
            "hello  world",
            7,
            MarkdownWordDeleteDirection::Backward,
            5..7,
        ),
        (
            "hello\tworld",
            6,
            MarkdownWordDeleteDirection::Backward,
            5..6,
        ),
        (
            "hello world",
            5,
            MarkdownWordDeleteDirection::Forward,
            5..11,
        ),
        (
            "hello  world",
            5,
            MarkdownWordDeleteDirection::Forward,
            5..7,
        ),
        ("foo!!!bar", 3, MarkdownWordDeleteDirection::Forward, 3..6),
        (
            "中文分组测试",
            2,
            MarkdownWordDeleteDirection::Backward,
            0..2,
        ),
        (
            "中文分组测试",
            2,
            MarkdownWordDeleteDirection::Forward,
            2..6,
        ),
        (
            "e\u{301}clair",
            2,
            MarkdownWordDeleteDirection::Backward,
            0..2,
        ),
        (
            "e\u{301}clair",
            2,
            MarkdownWordDeleteDirection::Forward,
            2..7,
        ),
        ("👩‍👩‍👧‍👦😀", 0, MarkdownWordDeleteDirection::Forward, 0..13),
        ("אבג דהו", 3, MarkdownWordDeleteDirection::Backward, 0..3),
        ("אבג דהו", 3, MarkdownWordDeleteDirection::Forward, 3..7),
        ("a_12!", 0, MarkdownWordDeleteDirection::Forward, 0..4),
        ("a\u{feff}b", 1, MarkdownWordDeleteDirection::Forward, 1..2),
        ("a\u{85}b", 1, MarkdownWordDeleteDirection::Forward, 1..2),
        ("a\u{feff} b", 1, MarkdownWordDeleteDirection::Forward, 1..3),
        ("a\u{85}!b", 1, MarkdownWordDeleteDirection::Forward, 1..3),
        ("", 0, MarkdownWordDeleteDirection::Forward, 0..0),
        ("x", 0, MarkdownWordDeleteDirection::Backward, 0..0),
        ("x", 1, MarkdownWordDeleteDirection::Forward, 1..1),
    ] {
        let (_directory, document, mut composer) = fixture(text);
        composer.begin_word_delete(direction, head).unwrap();
        assert_eq!(drive(&mut composer, tiny), expected, "{text:?}");
        // 查询不能改canonical或历史；重复终态仍必须沿同一原源返回相同真实范围。
        assert_eq!(document.generation(), 0);
        assert!(!document.is_dirty() && !document.can_undo() && !document.can_redo());
        assert_eq!(drive(&mut composer, tiny), expected);
    }
}

#[test]
fn real_line_boundaries_keep_cr_lf_and_cross_chunk_crlf_as_one_delete() {
    for eol in ["\n", "\r", "\r\n"] {
        let text = format!("first{eol}next");
        let next = 5 + eol.len();
        for (head, direction, expected) in [
            (5, MarkdownWordDeleteDirection::Backward, 0..5),
            (5, MarkdownWordDeleteDirection::Forward, 5..next),
            (next, MarkdownWordDeleteDirection::Backward, 5..next),
            (next, MarkdownWordDeleteDirection::Forward, next..next + 4),
        ] {
            let (_directory, _document, mut composer) = fixture(&text);
            composer.begin_word_delete(direction, head).unwrap();
            assert_eq!(
                drive(
                    &mut composer,
                    MarkdownLineComposeBudget {
                        bytes: 1,
                        scalars: 1,
                        records: 1,
                        metadata_units: 1,
                    }
                ),
                expected
            );
        }
    }
}

#[test]
fn two_mebibyte_original_word_is_complete_and_changes_history_once() {
    let length = 2 * 1024 * 1024;
    let text = format!("prefix {}!", "x".repeat(length));
    let (_directory, mut document, mut composer) = fixture(&text);
    let before = MarkdownTextSelection::caret(7 + length);
    document.set_selection(before).unwrap();
    composer
        .begin_word_delete(MarkdownWordDeleteDirection::Backward, before.head_utf16)
        .unwrap();
    let range = drive(&mut composer, MarkdownLineComposeBudget::default());
    assert_eq!(range, 7..7 + length);
    document
        .replace_utf16_range(range.clone(), "", MarkdownTextSelection::caret(range.start))
        .unwrap();
    assert_eq!(document.generation(), 1);
    let mut actual = Vec::new();
    document.write_to(&mut actual).unwrap();
    assert_eq!(actual, b"prefix !");
    assert_eq!(document.undo(), Some(before));
    assert!(!document.can_undo() && document.can_redo());
    actual.clear();
    document.write_to(&mut actual).unwrap();
    assert_eq!(actual, text.as_bytes());
    assert!(document.redo().is_some());
    assert!(!document.can_redo());
    actual.clear();
    document.write_to(&mut actual).unwrap();
    assert_eq!(actual, b"prefix !");
}

#[test]
fn a_single_multi_mebibyte_grapheme_never_becomes_a_chunk_boundary() {
    let marks = 1024 * 1024;
    let text = format!("!e{}?", "\u{301}".repeat(marks));
    let (_directory, _document, mut composer) = fixture(&text);
    composer
        .begin_word_delete(MarkdownWordDeleteDirection::Forward, 1)
        .unwrap();
    assert_eq!(
        drive(&mut composer, MarkdownLineComposeBudget::default()),
        1..2 + marks
    );
    let word = pending_delete(&composer);
    assert!(word.cluster.is_none()); // 完成后只剩范围标量，没有保留整簇字节或额外Source。
}

#[test]
fn cancel_only_the_pending_word_keeps_original_layout_worker_and_new_word_usable() {
    let (_directory, document, mut composer) = fixture("中文 原词组");
    composer
        .begin_word_delete(MarkdownWordDeleteDirection::Forward, 0)
        .unwrap();
    composer
        .step_word_delete(MarkdownLineComposeBudget {
            bytes: 1,
            scalars: 1,
            records: 1,
            metadata_units: 1,
        })
        .unwrap();
    composer.cancel_word_delete();
    composer
        .begin_word_delete(MarkdownWordDeleteDirection::Backward, document.utf16_len())
        .unwrap();
    assert_eq!(
        drive(&mut composer, MarkdownLineComposeBudget::default()),
        3..6
    );
    assert!(!document.is_dirty());
}

#[test]
fn cancellation_releases_the_real_original_gcb_resources() {
    let (_directory, document, mut composer) = fixture("中文 待取消的原词组");
    let mapping = document.original_backing_observer().unwrap();
    let resources = composer.worker_resource_observer();
    composer
        .begin_word_delete(MarkdownWordDeleteDirection::Forward, 0)
        .unwrap();
    // 先实际推进原GCB，让原owner确实持有已写缓冲，随后再取消同composer位。
    for _ in 0..2 {
        composer
            .step_word_delete(MarkdownLineComposeBudget::default())
            .unwrap();
    }
    composer.cancellation.cancel();
    assert!(matches!(
        composer.step_word_delete(MarkdownLineComposeBudget::default()),
        Err(MarkdownLineComposeError::Cancelled)
    ));
    let facts = resources.snapshot().unwrap().graphemes.unwrap();
    assert!(facts.metadata_owner_released);
    assert!(!facts.metadata_file_open && facts.metadata_buffer_bytes == 0);
    drop((composer, document));
    assert!(resources.is_released());
    assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0);
}

#[test]
fn actual_source_eio_precedes_cancel_and_previously_ready_word_results() {
    let (_directory, document, mut composer) = fixture("中文 原源错误");
    composer
        .begin_word_delete(MarkdownWordDeleteDirection::Forward, 0)
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
        composer.step_word_delete(MarkdownLineComposeBudget::default()),
        Err(MarkdownLineComposeError::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
    assert!(!document.can_undo() && !document.can_redo());
}

#[test]
fn combining_interior_caret_is_rejected_before_any_partial_grapheme_edit() {
    let (_directory, document, mut composer) = fixture("e\u{301}clair");
    composer
        .begin_word_delete(MarkdownWordDeleteDirection::Backward, 1)
        .unwrap();
    let mut failure = None;
    for _ in 0..64 {
        match composer.step_word_delete(MarkdownLineComposeBudget::default()) {
            Ok(step) => assert!(matches!(step.answer, MarkdownWordDeleteAnswer::Yielded)),
            Err(error) => {
                failure = Some(error);
                break;
            }
        }
    }
    assert!(matches!(
        failure,
        Some(MarkdownLineComposeError::InvalidRequest)
    ));
    assert!(!document.is_dirty() && !document.can_undo());
}
