//! 原视觉目录、实际 Arabic 字体、原 GCB 与正文分类的完整业务回归。

use super::*;
use crate::{MarkdownComposedVisualWordAnswer as WordAnswer, MarkdownComposedVisualWordQuery};

/// 一字节/一记录量子覆盖 UTF8 半标量和目录续算；这里只返回原真正终点或明确缺前缀。
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
/// RTL 的屏幕左向必须增加此真实 run 的逻辑位置；整词与 leading-space 的终点不能借反向逻辑动作代替。
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
/// 首个 RTL 段决定本次前后流向；跨 CRLF 后即使段 base 改变，也不能反转原动作或伪造 BOF。
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
/// ZWNJ 保实际 Arabic 字素与独立整形边界；首个软行结束仍缺后继，完整词不能在该视口变成 BOF。
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
/// 组合字符保持原 GCB，CRLF 与空行来自已认证控制；leading-space 不得被视口或字节量子截断。
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
/// 原 writer 的新 prefix 继续已分类空白；同 Source 的另一 writer 不能复用它的宽度/顺序权限。
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
/// UTF8 分类暂停和已完成缓存都保原弱 Font；实际注册 owner 归还后不得再交旧视觉终点。
fn visual_word_rejects_actual_font_retirement_before_and_after_completion() {
    for complete in [false, true] {
        let mut fixture = Fixture::new("سلام عالم");
        use_arabic_font(&mut fixture);
        let mut output = super::output(&fixture);
        let mut query = output
            .visual_word_query(0, MarkdownComposedLineAffinity::Downstream, None, false)
            .unwrap();
        if complete {
            assert!(matches!(
                finish_word(&mut output, &mut query).0,
                WordAnswer::Found { utf16: 4, .. }
            ));
        } else {
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
/// 原 opaque 控制只交回展开需求；不能按数值跳过隐区后再分类或把隐藏内容当空白。
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
/// 真 Source 回收 EIO 高于同时取消；视觉查询只持弱进度，失败必须归还原输出 Reader。
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
