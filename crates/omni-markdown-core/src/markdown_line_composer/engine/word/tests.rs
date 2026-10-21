//! Real synthetic files exercise word ranges, one-edit history and original source failures.

use super::*;
use crate::{MarkdownDocumentBuffer, MarkdownTextSelection};

/// Observe the real pending delete, without confusing a later group-selection action with it.
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
    // Each fixture has a private actual UTF-8 file and one captured layout source.
    // It owns only synthetic data; no user workspace is read or modified.
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
    // Bound fixture driving by its actual source size, covering GCB generation,
    // search and classification without changing production work quotas.
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

/// Write the large word in 4-KiB blocks without first creating a whole-word expected String.
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
    // The finite prefix/suffix are independent stopping characters. Only a single
    // fixed-size block is retained while generating the long original word.
    file.write_all(b"prefix ").unwrap();
    let block = [b'x'; crate::MARKDOWN_LAYOUT_SOURCE_BYTES];
    for _ in 0..length / block.len() {
        file.write_all(&block).unwrap();
    }
    file.write_all(&block[..length % block.len()]).unwrap();
    file.write_all(b"!").unwrap();
    drop(file); // Retire the real writer before consuming the same immutable source and tree summaries.
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
    // Both directions must reach the same complete long word using bounded actual
    // source work; a correct range alone cannot prove that interior nodes were skipped.
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
            // Accumulate reported work from the real operation, not a recorder or
            // replacement source. Each quantum retains all original budget dimensions.
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
            // Range discovery must be read-only and must release the actual worker
            // and original mapping after their final strong owners are dropped.
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
        } // This range query does not claim edit, native input-method or whole-product acceptance.
    }
}

#[test]
fn ascii_original_span_and_partial_leaf_progress_with_one_unit_budgets() {
    // Unit quotas force continuation through original spans and partial leaves.
    // Restarting at the root on every yield cannot finish within this finite bound.
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
        assert_eq!(ready, Some(7..7 + length)); // The result must cover the whole original word.
        assert!(bytes <= 2 * crate::MARKDOWN_LAYOUT_SOURCE_BYTES + 4);
        assert!(!document.can_undo());
    }
}

#[test]
fn ascii_skip_keeps_space_punctuation_underscore_and_non_ascii_gcb_rules() {
    // Printable ASCII is not one uniform word class. Independent expected ranges
    // also preserve adjacent CRLF and combining-mark boundaries across tiny quanta.
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
    } // Local ASCII facts cannot split an original mark or CRLF outside that local run.
}

#[test]
fn cancelling_a_live_ascii_tree_cursor_releases_the_original_worker_and_mapping() {
    // Wait for the actual tree cursor to exist before cancelling; cancelling an
    // unstarted request would not exercise release of its retained original nodes.
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
    assert!(entered); // The in-flight cursor really retains original tree nodes at this point.
    composer.cancel_word_delete();
    drop((composer, document));
    assert!(resources.is_released());
    assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0);
}

#[test]
fn live_ascii_cursor_keeps_source_eio_before_cancellation_and_really_releases() {
    // Establish a live original cursor, then consume the existing reclaim EIO
    // injection through an actual resident-source operation before cancellation.
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
    // The original source failure must remain the first cause when cancellation
    // arrives later, and retiring the owner must still release real resources.
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
    // A second capture of the same document is foreign authority. Keep a second
    // genuine original cursor only to compare continued public operation results.
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
        ); // Rejection must not consume the original cursor's node or byte continuation.
        if actual.complete {
            endpoint = Some(actual.position);
            break;
        } // Metadata and byte budgets remain independent while progressing through the real tree.
    }
    assert_eq!(endpoint, Some(4)); // Both cursors must actually reach the space after "same".
    assert_eq!(document.generation(), 0);
    assert!(!document.can_undo()); // Neither rejection nor range discovery changes the document history.
}

#[test]
fn actual_gcb_groups_keep_chinese_emoji_combining_rtl_and_space_deletion_rules() {
    // These independent expected ranges describe original word-class behavior,
    // including Chinese, full emoji clusters, marks, RTL and whitespace policy.
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
        // Queries cannot edit the canonical document or history. A repeated Ready
        // result must still validate the same original source and return its range.
        assert_eq!(document.generation(), 0);
        assert!(!document.is_dirty() && !document.can_undo() && !document.can_redo());
        assert_eq!(drive(&mut composer, tiny), expected);
    }
}

#[test]
fn real_line_boundaries_keep_cr_lf_and_cross_chunk_crlf_as_one_delete() {
    // Exercise both sides and both directions for each physical EOL spelling;
    // one-byte quanta must never manufacture a break between CR and LF.
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
    // Apply the discovered complete range through the actual document transaction,
    // then verify bytes and selection/history through real undo and redo.
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
    // The range query itself has not edited text; one explicit replacement now
    // creates exactly the single history operation exercised below.
    document
        .replace_utf16_range(range.clone(), "", MarkdownTextSelection::caret(range.start))
        .unwrap();
    assert_eq!(document.generation(), 1);
    let mut actual = Vec::new();
    document.write_to(&mut actual).unwrap();
    assert_eq!(actual, b"prefix !");
    // Undo restores the original selection and full synthetic bytes. Redo must
    // perform the same deletion without a second spurious history step.
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
    // A huge combining sequence is still one original grapheme. The expected
    // range is derived from this synthetic input, not from the queried GCB index.
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
}

#[test]
fn cancel_only_the_pending_word_keeps_original_layout_worker_and_new_word_usable() {
    // Cancel only the auxiliary word action after real progress, then start a new
    // action on the same healthy layout worker and original document.
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
    // Observe genuine GCB metadata after actual progress and global cancellation;
    // internal cancellation state alone would not prove file/buffer release.
    let (_directory, document, mut composer) = fixture("中文 待取消的原词组");
    let mapping = document.original_backing_observer().unwrap();
    let resources = composer.worker_resource_observer();
    composer
        .begin_word_delete(MarkdownWordDeleteDirection::Forward, 0)
        .unwrap();
    // Advance the original GCB owner far enough to acquire its real write buffer
    // before cancelling this composer's actual request.
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
    // First obtain a genuine Ready range. Consume the existing reclaim EIO
    // injection, then ensure a later call cannot reuse that formerly healthy result.
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
    // A scalar boundary inside a combining grapheme is not a valid word-action
    // start. Failure must leave canonical bytes and undo history untouched.
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

#[test]
fn ordinary_delete_uses_one_real_gcb_for_crlf_emoji_combining_and_rtl_with_tiny_budgets() {
    let text = "a\r\ne\u{301}👩‍👩‍👧‍👦🇨🇳אב!";
    // Independent expected clusters: ASCII, CRLF, e+mark, family ZWJ, RI flag,
    // two RTL letters and punctuation, with UTF-16 lengths 1/2/2/11/4/1/1/1.
    // Do not derive expected boundaries from the same GCB implementation under test.
    let boundaries = [0, 1, 3, 5, 16, 20, 21, 22, 23];
    for (index, head) in boundaries.iter().copied().enumerate() {
        for direction in [
            MarkdownWordDeleteDirection::Backward,
            MarkdownWordDeleteDirection::Forward,
        ] {
            let (_directory, document, mut composer) = fixture(text);
            composer.begin_grapheme_delete(direction, head).unwrap();
            let endpoint = if direction == MarkdownWordDeleteDirection::Backward {
                boundaries[index.saturating_sub(1)]
            } else {
                boundaries[(index + 1).min(boundaries.len() - 1)]
            };
            let actual = drive(
                &mut composer,
                MarkdownLineComposeBudget {
                    bytes: 1,
                    scalars: 1,
                    records: 1,
                    metadata_units: 1,
                },
            );
            assert_eq!(actual, head.min(endpoint)..head.max(endpoint));
            assert!(!document.is_dirty() && !document.can_undo());
        } // Both directions include true BOF/EOF; unit budgets must not split CRLF or emoji clusters.
    }
}

#[test]
fn ordinary_delete_keeps_a_multi_mebibyte_grapheme_streamed_and_releases_its_source() {
    use std::io::Write;

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("超长单簇.md");
    let marks = 1024 * 1024;
    // Generate the actual oversized grapheme in bounded blocks, so the test does
    // not create a second whole-body String merely to construct an expected range.
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(b"!e").unwrap();
    let chunk = "\u{301}".repeat(2048);
    for _ in 0..marks / 2048 {
        file.write_all(chunk.as_bytes()).unwrap();
    } // Each generated block is 4 KiB; the original single grapheme remains complete.
    file.write_all(b"?").unwrap();
    drop(file);
    let document = MarkdownDocumentBuffer::open(&path, 3 * 1024 * 1024).unwrap();
    let mapping = document.original_backing_observer().unwrap();
    let source = MarkdownLayoutTextSource::capture(&document);
    let mut composer = MarkdownLineComposer::new(
        source.clone(),
        &source.authority(),
        MarkdownLineFontCatalog::new(),
        640 * 64,
        MarkdownLineComposeCancellation::new(),
    )
    .unwrap();
    drop(source); // No spare test source owner may conceal the composer's actual release.
    let resources = composer.worker_resource_observer();
    for (direction, head) in [
        (MarkdownWordDeleteDirection::Forward, 1),
        (MarkdownWordDeleteDirection::Backward, marks + 2),
    ] {
        composer.begin_grapheme_delete(direction, head).unwrap();
        assert_eq!(
            drive(&mut composer, MarkdownLineComposeBudget::default()),
            1..marks + 2
        );
        composer.cancel_word_delete(); // Retire only this action; the next direction reuses the real GCB index.
    }
    drop((composer, document));
    assert!(resources.is_released());
    assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0);
}

#[test]
fn ordinary_delete_preserves_scalar_carets_inside_combining_zwj_and_crlf() {
    // Ordinary deletion preserves the original strict Previous/Next caret policy
    // for scalar positions inside clusters rather than snapping to a new caret.
    for (text, head, backward, forward) in [
        ("!e\u{301}?", 2, 1..2, 2..3),
        ("!\r\n?", 2, 1..2, 2..3),
        ("!👩‍👩‍👧‍👦?", 3, 1..3, 3..12),
        ("!👩‍👩‍👧‍👦?", 4, 1..4, 4..12),
    ] {
        for (direction, expected) in [
            (MarkdownWordDeleteDirection::Backward, backward),
            (MarkdownWordDeleteDirection::Forward, forward),
        ] {
            let (_directory, document, mut composer) = fixture(text);
            document.byte_offset_for_utf16(head).unwrap();
            composer.begin_grapheme_delete(direction, head).unwrap();
            assert_eq!(
                drive(
                    &mut composer,
                    MarkdownLineComposeBudget {
                        bytes: 1,
                        scalars: 1,
                        records: 1,
                        metadata_units: 1,
                    },
                ),
                expected
            );
            assert!(!document.is_dirty() && !document.can_undo());
        } // The expected ranges retain the existing Scene contract for the supplied scalar caret.
    }
}

#[test]
fn ordinary_delete_rejects_surrogate_interior_without_starting_a_cursor() {
    // Reject an interior UTF-16 surrogate before a valid original scalar request;
    // the failed coordinate must not consume or poison the later legitimate action.
    for direction in [
        MarkdownWordDeleteDirection::Backward,
        MarkdownWordDeleteDirection::Forward,
    ] {
        let (_directory, document, mut composer) = fixture("!😀?");
        assert!(matches!(
            composer.begin_grapheme_delete(direction, 2),
            Err(MarkdownLineComposeError::Source(
                MarkdownDocumentBufferError::NotUnicodeBoundary
            ))
        ));
        assert!(!document.is_dirty() && !document.can_undo());
        composer.begin_grapheme_delete(direction, 1).unwrap();
        let expected = if direction == MarkdownWordDeleteDirection::Forward {
            1..3
        } else {
            0..1
        };
        assert_eq!(
            drive(&mut composer, MarkdownLineComposeBudget::default()),
            expected
        ); // The invalid request cannot publish half a surrogate pair or consume the valid successor.
    }
}
