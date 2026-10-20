//! 真正 composer/Store 控制回归：隐藏正文仍在 canonical/mmap 内，只省略布局与内部命中。

use super::*;

/// 经原样式/控制响应完成有限 fixture，记录真实行而不是构造 Store 或伪造 measure permit。
fn folded_output(
    text: &str,
    hidden: std::ops::Range<usize>,
) -> (Fixture, MarkdownComposedLines, MarkdownLineRegisteredFont) {
    let fixture = Fixture::new(text);
    let (mut worker, font) =
        super::engine::composer(&fixture, 800 * 64, MarkdownLineComposeCancellation::new());
    worker
        .install_folded_ranges(std::slice::from_ref(&hidden))
        .unwrap();
    let mut folded = 0usize;
    let mut complete = false;
    for _ in 0..100_000 {
        let progress = worker.step(budget()).unwrap();
        assert!(
            progress.bytes <= 7
                && progress.scalars <= 3
                && progress.records <= 4
                && progress.metadata_units <= 2
        );
        match progress.answer {
            MarkdownLineComposeAnswer::Yielded | MarkdownLineComposeAnswer::LineReady(_) => {}
            MarkdownLineComposeAnswer::NeedStyle(request) => {
                let range = request.source_range();
                assert!(
                    range.end <= hidden.start || range.start >= hidden.end,
                    "隐藏正文不能送字体/整形: {range:?}"
                );
                worker
                    .provide_style(request.respond_registered(range, &font, style()).unwrap())
                    .unwrap();
            }
            MarkdownLineComposeAnswer::NeedsControl(request) => {
                if request.kind == MarkdownLineInlineControlKind::FoldedSection {
                    assert_eq!(request.source_utf16, hidden);
                    folded += 1;
                }
                worker
                    .provide_control(
                        request
                            .respond(
                                &font,
                                16.0,
                                MarkdownLineParagraphStyle {
                                    tab_stops: Vec::new(),
                                    default_tab_interval_26_6: Some(40 * 64),
                                },
                            )
                            .unwrap(),
                    )
                    .unwrap();
            }
            MarkdownLineComposeAnswer::Complete => {
                complete = true;
                break;
            }
            _ => panic!("真实有限折叠不能变成缺字、重形或未完成前沿"),
        }
    }
    assert!(complete);
    assert_eq!(folded, 1);
    let output = worker.into_output().unwrap();
    assert_eq!(output.source_range(), 0..fixture.source.utf16_len());
    (fixture, output, font)
}

#[test]
// 多行隐藏只能成为一个零字形控制片；原 canonical 范围和输出 reader 的真实归还必须保留。
fn folded_section_elides_real_lines_and_keeps_one_zero_glyph_canonical_part() {
    let text = "# H\r\nhidden one\r\nhidden two\r\n# N\r\nvisible";
    let hidden = 3..text.find("# N").unwrap();
    let (_fixture, mut output, _font) = folded_output(text, hidden.clone());
    let resources = output.resource_observer();
    let mut line = output.last_line().unwrap();
    let mut count = 0;
    let mut seen = 0;
    while let Some(actual) = line {
        count += 1;
        for index in 0..actual.facts().parts {
            let part = output.part(&actual, index).unwrap().unwrap();
            let facts = part.facts();
            if facts.kind == MarkdownComposedLinePartKind::FoldedSection {
                assert_eq!(facts.source_utf16, hidden);
                assert_eq!((facts.glyphs, facts.x_advance_26_6), (0, 0));
                seen += 1;
            } else if facts.kind == MarkdownComposedLinePartKind::Glyphs {
                assert!(
                    facts.source_utf16.end <= hidden.start
                        || facts.source_utf16.start >= hidden.end
                );
            }
        }
        line = output.previous_line(&actual).unwrap();
    }
    assert_eq!((count, seen), (3, 1));
    output.close().unwrap();
    let released = resources.snapshot();
    assert!(!released.file_open && released.mapped_bytes == 0 && !released.cleanup_failed);
}

#[test]
// 无终止符的真实 EOF 没有后继软行；左右边界可以跨隐区，内部位置不能获得 caret 或 GCB。
fn folded_to_unterminated_eof_visual_right_crosses_only_the_real_opaque_endpoint() {
    let (_fixture, mut output, _font) = folded_output("# H\nbody", 3..8);
    let line = output.last_line().unwrap().unwrap();
    assert_eq!(line.facts().ordinal, 0);
    let mut query = output
        .visual_query(3, MarkdownComposedLineAffinity::Downstream, None, true)
        .unwrap();
    let mut answer = MarkdownComposedVisualAnswer::Pending;
    for _ in 0..10000 {
        let step = output.step_visual(&mut query, 1).unwrap();
        assert!(step.records <= 1);
        answer = step.answer;
        if !matches!(answer, MarkdownComposedVisualAnswer::Pending) {
            break;
        }
    }
    assert!(matches!(
        answer,
        MarkdownComposedVisualAnswer::Found {
            utf16: 8,
            ordinal: 0,
            ..
        }
    ));
    let mut folded = None;
    for index in 0..line.facts().parts {
        let part = output.part(&line, index).unwrap().unwrap();
        if part.facts().kind == MarkdownComposedLinePartKind::FoldedSection {
            folded = Some(part);
        }
    }
    let part = folded.unwrap();
    assert_eq!(
        output.grapheme_ordinal_domain().unwrap(),
        crate::MarkdownGraphemeOrdinalDomain::FoldedVisible
    );
    assert!(matches!(
        output.grapheme_query(4, crate::MarkdownGraphemeQuery::IsBoundary),
        Err(MarkdownLineComposeError::FoldedRange(range)) if range == (3..8)
    ));
    assert!(matches!(
        output.grapheme_ordinal_answer(5),
        Err(MarkdownLineComposeError::FoldedRange(range)) if range == (3..8)
    ));
    assert!(matches!(
        output.grapheme_query(3, crate::MarkdownGraphemeQuery::Next).unwrap(),
        crate::MarkdownGraphemeAnswer::Known(Some(boundary)) if boundary.utf16 == 8
    )); // typed 隐区拒绝不得关闭健康 reader，下一次真实端点查询仍能消费同一输出。
    assert!(matches!(
        output.visual_grapheme_query(&part, 4, true),
        Err(MarkdownLineComposeError::InvalidRequest)
    ));
    let mut query = output.visual_grapheme_query(&part, 3, true).unwrap();
    let mut answer = MarkdownComposedVisualGraphemeAnswer::Pending;
    for _ in 0..10000 {
        let step = output.step_visual_grapheme(&mut query, 1).unwrap();
        assert!(step.records <= 1);
        answer = step.answer;
        if !matches!(answer, MarkdownComposedVisualGraphemeAnswer::Pending) {
            break;
        }
    }
    assert!(matches!(
        answer,
        MarkdownComposedVisualGraphemeAnswer::Found { utf16: 8, .. }
    ));
    output.close().unwrap();
}

#[test]
// 只有源中真实存在最后 CRLF 时才保留空 EOF 行，折叠不得新增或吞掉这个原段落。
fn folded_real_trailing_crlf_preserves_the_original_empty_eof_paragraph() {
    let (_fixture, mut output, _font) = folded_output("# H\r\nbody\r\n", 3..11);
    let empty = output.last_line().unwrap().unwrap();
    assert_eq!(empty.facts().source_utf16, 11..11);
    assert_eq!(empty.facts().ordinal, 1);
    let header = output.previous_line(&empty).unwrap().unwrap();
    assert_eq!(header.facts().source_utf16, 0..11);
    output.close().unwrap();
}

#[cfg(feature = "paged-shaping-frontier")]
#[test]
// 巨型 RTL、组合字符和 emoji 隐区不得送进 Script/UBA/GCB 迁入；原 composer 只发布实际三行。
fn folded_giant_unicode_body_keeps_only_visible_lines_and_original_opaque_endpoints() {
    let body = "א中🙂e\u{301}\r\n".repeat(131_072);
    let text = format!("# H\r\n{body}# N\r\nafter");
    let hidden = 3..3 + 2 + body.encode_utf16().count();
    let (_fixture, mut output, _font) = folded_output(&text, hidden.clone());
    let mut line = output.last_line().unwrap();
    let mut lines = 0usize;
    while let Some(actual) = line {
        lines += 1;
        for index in 0..actual.facts().parts {
            let part = output.part(&actual, index).unwrap().unwrap();
            let facts = part.facts();
            if facts.kind == MarkdownComposedLinePartKind::FoldedSection {
                assert_eq!(facts.source_utf16, hidden);
            } else {
                assert!(
                    facts.source_utf16.end <= hidden.start
                        || facts.source_utf16.start >= hidden.end
                );
            }
        }
        line = output.previous_line(&actual).unwrap();
    }
    assert_eq!(lines, 3);
    let resources = output.resource_observer();
    output.close().unwrap();
    let released = resources.snapshot();
    assert!(!released.file_open && released.mapped_bytes == 0 && !released.cleanup_failed);
}
