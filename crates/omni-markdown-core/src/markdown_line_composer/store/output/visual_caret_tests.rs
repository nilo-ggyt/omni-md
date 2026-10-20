//! 实际字体、Line/Part与GCB目录的邻接合同；只在有限oracle内列举已知完整字素。

use super::*;
use crate::{
    MarkdownComposedCaretAssociation as Association, MarkdownComposedVisualGraphemeAnswer as Answer,
};
#[path = "trailing_whitespace_tests.rs"]
mod trailing_whitespace_tests;
#[cfg(feature = "paged-shaping-frontier")]
#[path = "visual_word_tests.rs"]
mod word_tests;

fn output(fixture: &Fixture) -> MarkdownComposedLines {
    let mut worker = MarkdownLineComposer::new(
        fixture.source.clone(),
        &fixture.source.authority(),
        fixture.catalog.clone(),
        10_000 * 64,
        fixture.cancellation.clone(),
    )
    .unwrap();
    while fixture.next_line(&mut worker).is_some() {}
    worker.into_output().unwrap()
}

/// 仅有限真实业务夹具推进到终点；生产客户每个量子后回到原队列。
fn neighbor(
    output: &mut MarkdownComposedLines,
    part: &MarkdownComposedLinePart,
    at: usize,
    right: bool,
) -> Result<Answer> {
    let mut query = output.visual_grapheme_query(part, at, right)?;
    loop {
        let step = output.step_visual_grapheme(&mut query, 1)?;
        assert!(step.records <= 1);
        if step.answer != Answer::Pending {
            return Ok(step.answer);
        }
    }
}

fn use_arabic_font(fixture: &mut Fixture) {
    // 使用仓库已有真实Arabic字体，不以缺字或强制ASCII方向制造RTL成功。
    let bytes = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../vendor/harfrust/tests/fonts/in-house/SimpArabicTest.ttf"
    ));
    let name = read_fonts::FontRef::from_index(bytes, 0)
        .unwrap()
        .table_data(read_fonts::types::Tag::new(b"name"))
        .unwrap()
        .as_bytes()
        .to_vec();
    let font = crate::MarkdownLayoutFont::resolve_name_table_at_size(
        Arc::new(bytes.as_slice()),
        &name,
        &[],
        16.0,
    )
    .unwrap();
    fixture.font = fixture.catalog.register(&font).unwrap();
}

/// U+0600 必须使用确实覆盖该字符的原字体；旧 PUA Arabic 样本只有历史字母映射。
fn use_prepend_font(fixture: &mut Fixture) {
    // 未改造的 Amiri 1.002，源提交 04d40ee68cc6b8cb6870eca81a8a7451165aa95a。
    // https://github.com/aliftype/amiri/releases/download/1.002/Amiri-1.002.zip
    // 原 TTF SHA256：ab391c4147d054c48976e98322ad0eefe1427aa0e0502a12a4c75d80a70cfcd7。
    // 必要 OFL 与字体同目录；仅作为测试编译输入，不装入正式应用或复制系统字体。
    let bytes = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fonts/amiri/Amiri-Regular.ttf"
    ));
    let name = read_fonts::FontRef::from_index(bytes, 0)
        .unwrap()
        .table_data(read_fonts::types::Tag::new(b"name"))
        .unwrap()
        .as_bytes()
        .to_vec();
    let font = crate::MarkdownLayoutFont::resolve_name_table_at_size(
        Arc::new(bytes.as_slice()),
        &name,
        &[],
        16.0,
    )
    .unwrap();
    fixture.font = fixture.catalog.register(&font).unwrap();
}

#[test]
fn exact_gcb_steps_preserve_combining_and_actual_rtl_part_directions() {
    for (text, boundaries, rtl) in [
        ("Ae\u{301}xy", vec![0, 1, 3, 4, 5], false),
        ("سلام", vec![0, 1, 2, 3, 4], true),
    ] {
        let mut fixture = Fixture::new(text);
        if rtl {
            use_arabic_font(&mut fixture);
        }
        let mut output = self::output(&fixture);
        let line = output.last_line().unwrap().unwrap();
        assert_eq!(line.facts().ordinal, 0);
        assert_eq!(output.paragraph_base_level(&line).unwrap(), u8::from(rtl));
        for ordinal in 0..line.facts().parts {
            let part = output.part(&line, ordinal).unwrap().unwrap();
            if part.facts().kind != crate::MarkdownComposedLinePartKind::Glyphs {
                continue;
            }
            assert_eq!(part.facts().level & 1 != 0, rtl);
            let range = part.facts().source_utf16.clone();
            let actual: Vec<_> = boundaries
                .iter()
                .copied()
                .filter(|at| range.start <= *at && *at <= range.end)
                .collect();
            for pair in actual.windows(2) {
                for (at, expected, right, association) in [
                    (pair[0], pair[1], !rtl, Association::Before),
                    (pair[1], pair[0], rtl, Association::After),
                ] {
                    assert_eq!(
                        neighbor(&mut output, &part, at, right).unwrap(),
                        Answer::Found {
                            utf16: expected,
                            side: crate::MarkdownComposedCaretSide {
                                level: part.facts().level,
                                association
                            }
                        }
                    );
                }
            }
            assert_eq!(
                neighbor(&mut output, &part, range.end, !rtl).unwrap(),
                Answer::Edge
            );
            assert_eq!(
                neighbor(&mut output, &part, range.start, rtl).unwrap(),
                Answer::Edge
            );
        }
        let observer = output.resource_observer();
        output.close().unwrap();
        assert!(!observer.snapshot().file_open && !observer.snapshot().cleanup_failed);
    }
}

#[test]
fn actual_rtl_ls_and_new_paragraph_empty_lines_keep_distinct_original_bases() {
    for (text, empty_base) in [("سلام\u{2028}", 1), ("سلام\n", 0)] {
        let mut fixture = Fixture::new(text);
        use_arabic_font(&mut fixture);
        let mut output = self::output(&fixture);
        let empty = output.last_line().unwrap().unwrap();
        assert!(empty.facts().source_utf16.is_empty());
        assert_eq!(output.paragraph_base_level(&empty).unwrap(), empty_base);
        let previous = output.previous_line(&empty).unwrap().unwrap();
        assert_eq!(output.paragraph_base_level(&previous).unwrap(), 1);
    } // LS的空视觉行仍属原RTL段；真正B之后的新空段按原策略为0，不互相套用。
}

#[test]
fn foreign_part_and_source_first_error_cannot_authorize_visual_navigation() {
    let fixture = Fixture::new("abc");
    let mut original = self::output(&fixture);
    let line = original.last_line().unwrap().unwrap();
    let part = original.part(&line, 0).unwrap().unwrap();
    let mut foreign = self::output(&fixture);
    assert!(matches!(
        neighbor(&mut foreign, &part, 0, true),
        Err(Error::SourceMismatch)
    ));
    // 原Source真正失败后，已认证part的缓存和同时取消都不能掩盖首EIO。
    fixture
        .document
        .text_snapshot()
        .visit_byte_range(0..1, |_| Ok(()))
        .unwrap();
    fixture.document.fail_next_original_reclaim_for_test();
    assert!(
        matches!(fixture.document.reclaim_original_read_windows_for_test(),
        Err(crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(errno)) if errno == libc::EIO)
    );
    fixture.cancellation.cancel();
    assert!(matches!(neighbor(&mut original,&part,0,true),
        Err(Error::Source(crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(errno))) if errno == libc::EIO));
    let resources = original.resource_observer();
    drop((original, foreign, fixture));
    let actual = resources.snapshot();
    assert!(
        !actual.file_open
            && actual.source_payload_released
            && actual.first_source_errno == Some(libc::EIO)
    );
}

#[test]
fn combining_interior_is_rejected_instead_of_becoming_a_visual_step() {
    let fixture = Fixture::new("e\u{301}x");
    let mut output = self::output(&fixture);
    let line = output.last_line().unwrap().unwrap();
    let part = output.part(&line, 0).unwrap().unwrap();
    assert!(matches!(
        neighbor(&mut output, &part, 1, true),
        Err(Error::InvalidRequest)
    ));
    assert!(matches!(
        neighbor(&mut output, &part, 0, true),
        Ok(Answer::Found { utf16: 2, .. })
    ));
}

/// 每回合只读一条原记录；预期来自有限原文，不从私有目录计数反推。
fn visual(
    output: &mut MarkdownComposedLines,
    head: usize,
    upstream: bool,
    right: bool,
) -> crate::MarkdownComposedVisualAnswer {
    let affinity = if upstream {
        crate::MarkdownComposedLineAffinity::Upstream
    } else {
        crate::MarkdownComposedLineAffinity::Downstream
    };
    let mut query = output.visual_query(head, affinity, None, right).unwrap();
    for _ in 0..32768 {
        let step = output.step_visual(&mut query, 1).unwrap();
        assert!(
            step.records <= 1,
            "Every line/part/item/GCB read must use this same quota"
        );
        if !matches!(step.answer, crate::MarkdownComposedVisualAnswer::Pending) {
            return step.answer;
        }
    }
    panic!("The finite visual directory query did not finish");
}

#[test]
fn quota_one_visual_crosses_real_crlf_and_empty_eof_without_inventing_a_scalar() {
    use crate::MarkdownComposedVisualAnswer as Visual;
    let fixture = Fixture::new("ae\u{301}\r\n\r\nz");
    let mut output = self::output(&fixture);
    for (head, upstream, right, expected, line, ordinal) in [
        (1, false, true, 3, 0..5, 0), // The combining mark remains inside its original GCB.
        (3, false, true, 5, 5..7, 1), // CRLF is one original boundary, not two character steps.
        (5, false, true, 7, 7..8, 2),
        (7, false, false, 5, 5..7, 1),
        (5, false, false, 3, 0..5, 0),
        (8, false, true, 8, 7..8, 2),
        (0, false, false, 0, 0..5, 0),
    ] {
        assert!(
            matches!(visual(&mut output,head,upstream,right),Visual::Found{utf16,line:actual,ordinal:actual_ordinal,..} if utf16==expected && actual==line && actual_ordinal==ordinal)
        );
    }
    let fixture = Fixture::new("abc\r\n");
    let mut output = self::output(&fixture);
    assert!(
        matches!(visual(&mut output,3,false,true),Visual::Found{utf16:5,line,ordinal:1,..} if line== (5..5))
    );
    assert!(
        matches!(visual(&mut output,5,false,true),Visual::Found{utf16:5,line,ordinal:1,..} if line== (5..5))
    );
    assert!(matches!(
        visual(&mut output, 5, false, false),
        Visual::Found {
            utf16: 3,
            ordinal: 0,
            ..
        }
    ));
}

#[test]
fn quota_one_visual_uses_actual_soft_lines_and_unknown_prefix_is_not_eof() {
    use crate::MarkdownComposedVisualAnswer as Visual;
    let fixture = Fixture::new("a a a a a a");
    let mut composer = fixture.composer();
    let first = fixture.next_line(&mut composer).unwrap();
    let mut prefix = composer.publish_ready_prefix().unwrap().unwrap();
    assert!(matches!(
        visual(&mut prefix, first.source_utf16.end, true, true),
        Visual::PrefixNotCovered
    ));
    drop(prefix);
    while fixture.next_line(&mut composer).is_some() {}
    let mut output = composer.into_output().unwrap();
    let target = visual(&mut output, first.source_utf16.end, true, true);
    assert!(
        matches!(target,Visual::Found{utf16,line,ordinal:1,..} if utf16==first.source_utf16.end+1 && line.start==first.source_utf16.end)
    );
    assert!(
        matches!(visual(&mut output,first.source_utf16.end,false,false),Visual::Found{utf16,ordinal:0,..} if utf16==first.source_utf16.end-1)
    ); // 软行入口没有字符：原CM的一次Left/Right必须越过真实ASCII GCB，不先消耗亲和性动作。
}

#[test]
fn quota_one_visual_keeps_prepend_graphemes_across_actual_bidi_shaping_parts() {
    use crate::MarkdownComposedVisualAnswer as Visual;
    for text in ["\u{600}  ", "\u{600}ا"] {
        // U+0600 与后一个标量属于同一 GCB，但原 UBA 可以在其内部产生不同 level 的整形项。
        // 使用实际字体和 Composer，不能靠改 level、伪造 glyph 或目录记录构造这条边界。
        let mut fixture = Fixture::new(text);
        use_prepend_font(&mut fixture);
        let source = fixture.source.resource_observer();
        let mapping = fixture.source.original_backing_observer().unwrap();
        let mut output = self::output(&fixture);
        let resources = output.resource_observer();
        let last = text.encode_utf16().count();

        // 用户不能把簇内部的标量位置作为箭头起点；拒绝只结束这一次无效请求。
        let mut invalid = output
            .visual_query(
                1,
                crate::MarkdownComposedLineAffinity::Downstream,
                None,
                true,
            )
            .unwrap();
        let mut rejected = false;
        for _ in 0..32768 {
            match output.step_visual(&mut invalid, 1) {
                Err(Error::InvalidRequest) => {
                    rejected = true;
                    break;
                }
                Ok(step) => {
                    assert!(step.records <= 1);
                    assert_eq!(step.answer, Visual::Pending);
                }
                Err(cause) => panic!("簇内部输入不得污染健康 Output：{cause:?}"),
            }
        }
        assert!(rejected, "真实 GCB 校验必须明确拒绝 UTF16=1");

        // 两个屏幕方向都经原真实视觉项和同一条 GCB 目录续算，任何终点都不能停在内部 seam。
        // 对 RTL 样本保留原方向与亲和性，不拿源下标的正负方向代替屏幕方向。
        let mut reached_other_boundary = false;
        for at in [0, 2, last] {
            for right in [false, true] {
                let answer = visual(&mut output, at, false, right);
                let Visual::Found { utf16, .. } = answer else {
                    panic!("完整短文的实际视觉动作必须完成：text={text:?} answer={answer:?}");
                };
                assert!(utf16 == 0 || utf16 == 2 || utf16 == last);
                reached_other_boundary |= at == 0 && utf16 == 2;
                if text.ends_with("  ") {
                    let expected = match (at, right) {
                        (0, false) => 0,
                        (0, true) | (3, false) => 2,
                        (2, false) => 0,
                        (2, true) | (3, true) => 3,
                        _ => unreachable!("该有限原文只含三个真实 GCB 端点"),
                    };
                    assert_eq!(utf16, expected, "head={at}, right={right}");
                }
            }
        }
        assert!(
            reached_other_boundary,
            "原同一字素必须能通过实际箭头动作越过"
        );

        // 导航不能改 canonical、历史或磁盘；仍存活的弱查询也不能保活 Reader 和原文件映射。
        let path = fixture._directory.path().join("真实行查找.md");
        assert_eq!(std::fs::read(path).unwrap(), text.as_bytes());
        assert!(!fixture.document.is_dirty() && !fixture.document.can_undo());
        output.close().unwrap();
        drop(fixture);
        let actual = resources.snapshot();
        assert!(!actual.file_open && actual.mapped_bytes == 0 && !actual.cleanup_failed);
        assert!(source.payload_released());
        assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
        drop(invalid);
    }
}

#[test]
fn paused_visual_query_has_no_resource_claim_and_foreign_cancel_or_source_error_rejects_it() {
    let fixture = Fixture::new("abc\r\nz");
    let mut original = self::output(&fixture);
    let mut query = original
        .visual_query(
            1,
            crate::MarkdownComposedLineAffinity::Downstream,
            None,
            true,
        )
        .unwrap();
    assert_eq!(original.step_visual(&mut query, 1).unwrap().records, 1); // A real line tail was read; its head remains pending.
    let mut foreign = self::output(&fixture);
    assert!(matches!(
        foreign.step_visual(&mut query, 1),
        Err(Error::SourceMismatch)
    ));
    let receipt = original.resource_observer();
    let source = fixture.source.resource_observer();
    drop(original);
    drop(foreign);
    drop(fixture);
    assert!(!receipt.snapshot().file_open && source.payload_released());
    drop(query); // The still-live query did not retain the Source, reader or font owner.

    let cancelled = Fixture::new("abc");
    let mut cancelled_output = self::output(&cancelled);
    let receipt = cancelled_output.resource_observer();
    let mut cancelled_query = cancelled_output
        .visual_query(
            1,
            crate::MarkdownComposedLineAffinity::Downstream,
            None,
            true,
        )
        .unwrap();
    cancelled_output
        .step_visual(&mut cancelled_query, 1)
        .unwrap();
    cancelled.cancellation.cancel();
    assert!(matches!(
        cancelled_output.step_visual(&mut cancelled_query, 1),
        Err(Error::Cancelled)
    ));
    assert!(!receipt.snapshot().file_open);
    drop((cancelled_query, cancelled_output, cancelled));

    let fixture = Fixture::new("abc");
    let mut original = self::output(&fixture);
    let mut query = original
        .visual_query(
            1,
            crate::MarkdownComposedLineAffinity::Downstream,
            None,
            true,
        )
        .unwrap();
    original.step_visual(&mut query, 1).unwrap();
    fixture
        .document
        .text_snapshot()
        .visit_byte_range(0..1, |_| Ok(()))
        .unwrap();
    fixture.document.fail_next_original_reclaim_for_test();
    assert!(
        fixture
            .document
            .reclaim_original_read_windows_for_test()
            .is_err()
    );
    fixture.cancellation.cancel();
    assert!(
        matches!(original.step_visual(&mut query,1),Err(Error::Source(crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(errno))) if errno==libc::EIO)
    );
    assert!(!original.resource_observer().snapshot().file_open);
}

#[test]
fn queued_crlf_coordinates_and_punctuation_left_use_the_original_gcb_endpoints() {
    use crate::MarkdownComposedVisualAnswer as Visual;
    let prefix = format!("b{}", "a\r\n".repeat(32));
    let fixture = Fixture::new(&format!("{prefix}{}", "Xa".repeat(128)));
    let mut output = self::output(&fixture);
    let mut head = 1;
    let mut upstream = false;
    let mut side = None;
    for index in 0..64 {
        let affinity = if upstream {
            crate::MarkdownComposedLineAffinity::Upstream
        } else {
            crate::MarkdownComposedLineAffinity::Downstream
        };
        let mut query = output.visual_query(head, affinity, side, true).unwrap();
        let answer = loop {
            let step = output.step_visual(&mut query, 1).unwrap();
            assert!(step.records <= 1);
            if !matches!(step.answer, Visual::Pending) {
                break step.answer;
            }
        };
        let Visual::Found {
            utf16,
            side: next_side,
            line,
            ..
        } = answer
        else {
            panic!("有限原文的真实邻行必须存在");
        };
        let expected = if index % 2 == 0 {
            index / 2 * 3 + 2
        } else {
            (index / 2 + 1) * 3 + 1
        };
        assert_eq!(utf16, expected, "原CRLF不得多走或少走一次GCB");
        head = utf16;
        upstream = !line.is_empty() && head == line.end;
        side = Some(next_side);
    }
    assert_eq!(head, 97);
    drop((output, fixture));
    let fixture = Fixture::new(&format!("{prefix}!{}", "Xa".repeat(128)));
    // 真实20pt行宽使!后的合法断点与不可断长词竞争，复现原生ASCII尾部独占短行。
    // 宽窗口不会经过软行入口，不能覆盖之前只换亲和性却没有越过!的真实回归。
    let mut composer = fixture.composer();
    while fixture.next_line(&mut composer).is_some() {}
    let mut output = composer.into_output().unwrap();
    assert!(
        matches!(
            visual(&mut output, 98, false, false),
            Visual::Found { utf16: 97, line, ordinal:32, .. } if line == (97..98)
        ),
        "新Source中的!必须在一次实际左移中越过"
    );
}
