//! Behavior through the original visual directory, actual Arabic fonts, grapheme boundaries, and Source classification.

use super::*;
use crate::{MarkdownComposedVisualWordAnswer as WordAnswer, MarkdownComposedVisualWordQuery};

/// One-byte/record quanta cover partial UTF-8 scalars and directory continuation; return an actual endpoint or explicit missing prefix.
fn finish_word(
    output: &mut MarkdownComposedLines,
    query: &mut MarkdownComposedVisualWordQuery,
) -> (WordAnswer, usize) {
    let mut bytes = 0;
    for _ in 0..100_000 {
        let step = output
            .step_visual_word(
                query,
                MarkdownLineComposeBudget {
                    bytes: 1,
                    scalars: 1,
                    records: 1,
                    metadata_units: 1,
                },
            )
            .unwrap();
        assert!(
            step.bytes <= 1 && step.scalars <= 1 && step.records <= 1 && step.metadata_units <= 1
        );
        bytes += step.bytes;
        if step.answer != WordAnswer::Pending {
            return (step.answer, bytes);
        }
    }
    panic!("有限原视觉词组没有完成");
}

#[test]
/// Screen-left in this actual RTL run increases logical position; complete-word/leading-space endpoints cannot use a substituted logical action.
fn visual_word_rtl_preserves_real_screen_direction_and_whole_groups() {
    let mut fixture = Fixture::new("سلام عالم");
    use_arabic_font(&mut fixture);
    let mut output = super::output(&fixture);
    for (head, right, expected) in [
        (0, false, 4),
        (4, false, 9),
        (9, true, 5),
        (5, true, 0),
        (0, true, 0),
        (9, false, 9),
    ] {
        let mut query = output
            .visual_word_query(head, MarkdownComposedLineAffinity::Downstream, None, right)
            .unwrap();
        let (answer, _) = finish_word(&mut output, &mut query);
        assert!(
            matches!(answer, WordAnswer::Found { utf16, .. } if utf16 == expected),
            "RTL head={head}, right={right}, actual={answer:?}"
        );
    }
    let resources = output.resource_observer();
    output.close().unwrap();
    assert!(!resources.snapshot().file_open && !resources.snapshot().cleanup_failed);
}

#[test]
/// The initial RTL paragraph fixes this query's flow; a new base after CRLF cannot reverse its action or manufacture BOF.
fn visual_word_keeps_its_original_flow_when_crossing_a_different_paragraph_base() {
    let mut fixture = Fixture::new("سلام\r\n1234");
    use_arabic_font(&mut fixture);
    let mut output = super::output(&fixture);
    for (head, expected) in [(4, 10), (6, 0)] {
        let mut query = output
            .visual_word_query(head, MarkdownComposedLineAffinity::Downstream, None, false)
            .unwrap();
        let (answer, _) = finish_word(&mut output, &mut query);
        assert!(
            matches!(answer, WordAnswer::Found { utf16, .. } if utf16 == expected),
            "跨 base 的屏幕 Left head={head}, actual={answer:?}"
        );
    }
}

#[test]
/// ZWNJ retains actual Arabic grapheme/shaping boundaries; an incomplete soft-line prefix cannot turn the whole word into a false document boundary.
fn visual_word_rtl_crosses_real_soft_lines_and_continues_the_same_prefix() {
    let text = "س\u{200c}".repeat(24);
    let mut fixture = Fixture::new(&text);
    use_arabic_font(&mut fixture);
    let mut composer = fixture.composer();
    let first = fixture.next_line(&mut composer).unwrap();
    assert!(
        first.source_utf16.end > 0 && first.source_utf16.end < 48,
        "必须实际折成多个软行，而非大宽度或整词溢出：{first:?}"
    );
    let mut prefix = composer.publish_ready_prefix().unwrap().unwrap();
    let mut query = prefix
        .visual_word_query(0, MarkdownComposedLineAffinity::Downstream, None, false)
        .unwrap();
    let mut bytes = 0;
    let mut missing = 0;
    for _ in 0..128 {
        let (answer, actual) = finish_word(&mut prefix, &mut query);
        bytes += actual;
        match answer {
            WordAnswer::PrefixNotCovered => {
                // Return the old reader before publishing another actual prefix.
                // The same query keeps classified progress without owning output.
                missing += 1;
                let old = prefix.resource_observer();
                drop(prefix);
                assert!(
                    !old.snapshot().file_open,
                    "词组进度不得保活已交还的旧 Reader"
                );
                fixture
                    .next_line(&mut composer)
                    .expect("缺后继的原软行必须由同一 Composer 真正完成");
                prefix = composer.publish_ready_prefix().unwrap().unwrap();
            }
            WordAnswer::Found { utf16: 48, .. } => {
                // A complete endpoint must include all previously missing soft
                // lines once; reversing direction then uses that real directory.
                assert!(missing > 0, "必须实际经过 prefix 不足，不能只测完整短输出");
                assert_eq!(
                    bytes,
                    text.len(),
                    "每个已分类簇只读一次，不因新 prefix 重扫长词"
                );
                let mut backward = prefix
                    .visual_word_query(48, MarkdownComposedLineAffinity::Downstream, None, true)
                    .unwrap();
                let (answer, backward_bytes) = finish_word(&mut prefix, &mut backward);
                assert!(
                    matches!(answer, WordAnswer::Found { utf16: 0, .. }),
                    "同一真实软行目录的屏幕 Right 必须回到词首：{answer:?}"
                );
                assert_eq!(backward_bytes, text.len());
                return;
            }
            answer => panic!("首软行不是真 BOF/EOF，也不是词界：{answer:?}"),
        }
    }
    panic!("同一 writer 的真实 RTL 长词未完成");
}

#[test]
/// Combining sequences retain their graphemes and verified CRLF/empty-line controls; viewport/byte quanta must not truncate leading-space classification.
fn visual_word_crosses_actual_crlf_and_combining_boundaries() {
    let fixture = Fixture::new("ae\u{301}\r\n\r\nz");
    let mut output = super::output(&fixture);
    for (head, right, expected) in [(1, true, 3), (3, true, 8), (7, false, 0)] {
        let mut query = output
            .visual_word_query(head, MarkdownComposedLineAffinity::Downstream, None, right)
            .unwrap();
        let (answer, _) = finish_word(&mut output, &mut query);
        assert!(
            matches!(answer, WordAnswer::Found { utf16, .. } if utf16 == expected),
            "head={head}, right={right}, actual={answer:?}"
        );
    }
}

#[test]
/// A new prefix from the same writer continues classified whitespace; another writer sharing Source cannot reuse its width/order authority.
fn visual_word_continues_the_same_writer_prefix_without_replaying_accepted_text() {
    let fixture = Fixture::new("a \u{2028}word");
    let mut composer = MarkdownLineComposer::new(
        fixture.source.clone(),
        &fixture.source.authority(),
        fixture.catalog.clone(),
        10_000 * 64,
        fixture.cancellation.clone(),
    )
    .unwrap();
    fixture.next_line(&mut composer).unwrap();
    let mut prefix = composer.publish_ready_prefix().unwrap().unwrap();
    let mut query = prefix
        .visual_word_query(1, MarkdownComposedLineAffinity::Downstream, None, true)
        .unwrap();
    let (answer, initial_bytes) = finish_word(&mut prefix, &mut query);
    assert_eq!(answer, WordAnswer::PrefixNotCovered);
    assert_eq!(initial_bytes, 1);
    let mut foreign_query = prefix
        .visual_word_query(1, MarkdownComposedLineAffinity::Downstream, None, true)
        .unwrap();
    let mut foreign = super::output(&fixture);
    assert!(matches!(
        foreign.step_visual_word(&mut foreign_query, MarkdownLineComposeBudget::default()),
        Err(Error::SourceMismatch)
    ));
    let old = prefix.resource_observer();
    drop(prefix);
    assert!(!old.snapshot().file_open, "弱词组游标不能保活旧 Reader");
    while fixture.next_line(&mut composer).is_some() {}
    let mut output = composer.into_output().unwrap();
    let (answer, remaining_bytes) = finish_word(&mut output, &mut query);
    assert!(matches!(answer, WordAnswer::Found { utf16: 7, .. }));
    assert_eq!(
        initial_bytes + remaining_bytes,
        5,
        "已分类的原空格不能因新增 prefix 再次读取"
    );
}

#[test]
/// Paused UTF-8 classification and completed results retain weak original font identity; dropping its registration revokes old visual endpoints.
fn visual_word_rejects_actual_font_retirement_before_and_after_completion() {
    for complete in [false, true] {
        let mut fixture = Fixture::new("سلام عالم");
        use_arabic_font(&mut fixture);
        let mut output = super::output(&fixture);
        let mut query = output
            .visual_word_query(0, MarkdownComposedLineAffinity::Downstream, None, false)
            .unwrap();
        if complete {
            // Even a cached complete answer depends on the original registration;
            // completion is not an independent lifetime claim on the font.
            assert!(matches!(
                finish_word(&mut output, &mut query).0,
                WordAnswer::Found { utf16: 4, .. }
            ));
        } else {
            // Pause after the first actual byte of a multibyte Arabic scalar,
            // before classification can finish its current grapheme.
            let mut suspended = false;
            for _ in 0..10_000 {
                let step = output
                    .step_visual_word(
                        &mut query,
                        MarkdownLineComposeBudget {
                            bytes: 1,
                            scalars: 1,
                            records: 1,
                            metadata_units: 1,
                        },
                    )
                    .unwrap();
                if step.bytes == 1 {
                    assert_eq!(step.scalars, 0, "Arabic 首个半标量必须仍在原续体");
                    suspended = true;
                    break;
                }
            }
            assert!(suspended);
        }
        let replacement = crate::MarkdownLayoutFont::resolve(
            Arc::new(
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../vendor/harfrust/benches/fonts/Roboto-Regular.ttf"
                ))
                .as_slice(),
            ),
            "Roboto-Regular",
            &[],
        )
        .unwrap();
        // Replacing the actual registered owner drops original font authority;
        // no private liveness bit or fabricated expired-font result is injected.
        fixture.font = fixture.catalog.register(&replacement).unwrap();
        let resources = output.resource_observer();
        assert!(matches!(
            output.step_visual_word(&mut query, MarkdownLineComposeBudget::default()),
            Err(Error::FontExpired)
        ));
        assert!(!resources.snapshot().file_open && !resources.snapshot().cleanup_failed);
    }
}

#[test]
/// Original opaque controls return fold-expansion requirements; do not skip numeric ranges or classify hidden text as whitespace.
fn visual_word_returns_the_original_fold_range_before_reading_hidden_text() {
    let fixture = Fixture::new("head\nאבג\nlast");
    let mut composer = MarkdownLineComposer::new(
        fixture.source.clone(),
        &fixture.source.authority(),
        fixture.catalog.clone(),
        10_000 * 64,
        fixture.cancellation.clone(),
    )
    .unwrap();
    composer
        .install_folded_ranges(std::slice::from_ref(&(4..9)))
        .unwrap();
    while fixture.next_line(&mut composer).is_some() {}
    let mut output = composer.into_output().unwrap();
    let mut query = output
        .visual_word_query(4, MarkdownComposedLineAffinity::Downstream, None, true)
        .unwrap();
    let (answer, bytes) = finish_word(&mut output, &mut query);
    assert_eq!(answer, WordAnswer::NeedsFoldExpansion(4..9));
    assert_eq!(bytes, 0, "原行末 opaque 之前无需读取任何正文来分类");
}

#[test]
/// Injected EIO from actual Source reclamation precedes cancellation; visual queries retain weak progress and failure returns the original output reader.
fn visual_word_keeps_original_source_error_before_cancellation() {
    let fixture = Fixture::new("word tail");
    let mut output = super::output(&fixture);
    let mut query = output
        .visual_word_query(0, MarkdownComposedLineAffinity::Downstream, None, true)
        .unwrap();
    fixture
        .document
        .text_snapshot()
        .visit_byte_range(0..1, |_| Ok(()))
        .unwrap();
    // Existing fault control injects EIO through actual source-window reclamation.
    // Do not call this a kernel I/O failure or let later cancellation hide it.
    fixture.document.fail_next_original_reclaim_for_test();
    assert_eq!(
        fixture.document.reclaim_original_read_windows_for_test(),
        Err(crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO))
    );
    fixture.cancellation.cancel();
    let resources = output.resource_observer();
    assert!(matches!(
        output.step_visual_word(&mut query, MarkdownLineComposeBudget::default()),
        Err(Error::Source(
            crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
    assert!(!resources.snapshot().file_open);
}
