//! Alignment endpoints from actual Composer/font/line output; compute bounded source oracles without changing production directories or glyphs.

use super::*;

const ONE: MarkdownLineComposeBudget = MarkdownLineComposeBudget {
    bytes: 1,
    scalars: 1,
    records: 1,
    metadata_units: 1,
};

/// Supply the fixture's actual left-tab grid, affecting measured geometry without replacing Tab characters with spaces.
fn compose(fixture: &Fixture) -> MarkdownComposedLines {
    let mut worker = MarkdownLineComposer::new(
        fixture.source.clone(),
        &fixture.source.authority(),
        fixture.catalog.clone(),
        10_000 * 64,
        fixture.cancellation.clone(),
    )
    .unwrap();
    for _ in 0..100_000 {
        match worker
            .step(MarkdownLineComposeBudget::default())
            .unwrap_or_else(|cause| {
                panic!(
                    "真实 Composer 尚未签发行：source_utf16={} cause={cause:?}",
                    fixture.source.utf16_len()
                )
            })
            .answer
        {
            MarkdownLineComposeAnswer::Yielded | MarkdownLineComposeAnswer::LineReady(_) => {}
            MarkdownLineComposeAnswer::NeedStyle(request) => {
                let range = request.source_range();
                worker
                    .provide_style(
                        request
                            .respond_registered(
                                range,
                                &fixture.font,
                                MarkdownLineComposeStyle {
                                    presentation: Default::default(),
                                    points: 16.0,
                                    language: None,
                                    features: Vec::new(),
                                },
                            )
                            .unwrap(),
                    )
                    .unwrap();
            }
            MarkdownLineComposeAnswer::NeedsControl(request) => {
                // Tab placement comes from the real paragraph control response;
                // retaining source Tabs exercises their measured alignment width.
                worker
                    .provide_control(
                        request
                            .respond(
                                &fixture.font,
                                16.0,
                                MarkdownLineParagraphStyle {
                                    tab_stops: Vec::new(),
                                    default_tab_interval_26_6: Some(64 * 64),
                                },
                            )
                            .unwrap(),
                    )
                    .unwrap();
            }
            MarkdownLineComposeAnswer::Complete => return worker.into_output().unwrap(),
            MarkdownLineComposeAnswer::NeedsResolution(reason) => {
                panic!("真实字体和原文尚未产生可对齐的完整行：{reason:?}");
            }
            MarkdownLineComposeAnswer::NeedsTabPlacement(reason) => {
                panic!("已提供真实左 Tab 网格仍需额外放置：{reason:?}");
            }
            #[cfg(feature = "paged-shaping-frontier")]
            MarkdownLineComposeAnswer::NeedsFrontier(reason) => {
                panic!("短原文不应缺少真实整形证明：{reason:?}");
            }
            #[cfg(all(
                feature = "s2-table-window-prototype",
                feature = "paged-shaping-frontier"
            ))]
            MarkdownLineComposeAnswer::NeedsTableRow(_) => panic!("普通文本夹具不能伪装成 Grid 行"),
        }
    }
    panic!("有限实际对齐夹具未完成排版");
}

/// One-byte quanta split multibyte scalars across steps; return actual Source bytes read rather than infer work from call counts.
fn trim(output: &mut MarkdownComposedLines, line: &MarkdownComposedLine) -> (usize, usize) {
    let mut query = output.begin_line_trailing_whitespace(line).unwrap();
    let mut bytes = 0;
    for _ in 0..100_000 {
        let step = output
            .step_line_trailing_whitespace(line, &mut query, ONE)
            .unwrap();
        assert!(
            step.bytes <= 1 && step.scalars <= 1 && step.records <= 1 && step.metadata_units <= 1
        );
        bytes += step.bytes;
        if let Some(end) = step.answer {
            // A completed query still validates original Source, without rereading text/directory to certify the same endpoint.
            let again = output
                .step_line_trailing_whitespace(line, &mut query, ONE)
                .unwrap();
            assert_eq!(
                (
                    again.answer,
                    again.bytes,
                    again.scalars,
                    again.records,
                    again.metadata_units
                ),
                (Some(end), 0, 0, 0, 0)
            );
            return (end, bytes);
        }
    }
    panic!("有限真实行的尾空白查询未完成");
}

#[test]
fn trailing_whitespace_quota_one_matches_real_unicode_tab_crlf_and_non_bmp_lines() {
    for text in [
        "ae\u{301}\u{1f16a}\u{2003}\u{a0}  \r\nnext \u{1f16b} \r\n",
        "a\t  \r\n\t\u{2003} \n",
        "a\u{200e}  \n",
        "",
    ] {
        let fixture = Fixture::new(text);
        let mapping = fixture.source.original_backing_observer();
        assert!(text.is_empty() || mapping.is_some());
        let source = fixture.source.resource_observer();
        let mut output = compose(&fixture);
        let resources = output.resource_observer();
        // Only these finite literals may materialize a source oracle. Its Unicode
        // whitespace result is independent of the output directory's own query.
        let units: Vec<_> = text.encode_utf16().collect();
        let mut current = output.last_line().unwrap();
        while let Some(line) = current {
            let facts = line.facts().clone();
            let original = String::from_utf16(&units[facts.source_utf16.clone()]).unwrap();
            let trimmed = original.trim_end_matches(char::is_whitespace);
            let expected = facts.source_utf16.start + trimmed.encode_utf16().count();
            let (end, bytes) = trim(&mut output, &line);
            assert_eq!(end, expected, "原文={text:?}，实际软行={facts:?}");
            // Quota one reads actual trailing whitespace and at most one stopping scalar, not earlier line text.
            let expected_bytes = original.len() - trimmed.len()
                + trimmed.chars().next_back().map_or(0, char::len_utf8);
            assert_eq!(bytes, expected_bytes);
            assert_eq!(
                line.facts(),
                &facts,
                "对齐事实不能重写完整 LineEnd 的 advance 或范围"
            );
            current = output.previous_line(&line).unwrap();
        }
        let path = fixture._directory.path().join("真实行查找.md");
        assert_eq!(std::fs::read(path).unwrap(), text.as_bytes());
        assert!(!fixture.document.is_dirty() && !fixture.document.can_undo());
        output.close().unwrap();
        drop(fixture);
        let actual = resources.snapshot();
        assert!(
            !actual.file_open
                && actual.mapped_bytes == 0
                && !actual.cleanup_failed
                && !actual.reader_failed
        );
        assert!(source.payload_released());
        if let Some(mapping) = mapping {
            assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
        } // An empty file may have no mapping; do not manufacture nonempty text for resource assertions.
    }
}

#[test]
fn trailing_whitespace_rtl_and_prepend_keep_actual_complete_grapheme_width() {
    for (text, expected) in [("سلام   ", 4), ("\u{600}  ", 2), ("\u{600}ب  ", 2)] {
        let mut fixture = Fixture::new(text);
        if text.starts_with('\u{600}') {
            use_prepend_font(&mut fixture); // U+0600 needs actual font coverage; a missing glyph cannot certify boundary success.
        } else {
            use_arabic_font(&mut fixture);
        }
        let mut output = compose(&fixture);
        let line = output.last_line().unwrap().unwrap();
        let facts = line.facts().clone();
        assert_eq!(trim(&mut output, &line).0, expected);
        assert_eq!(line.facts(), &facts);
        if text == "\u{600}  " {
            // U+0600 and the first space share one grapheme; trim only the second space without creating an interior caret.
            // Their actual levels belong to distinct shaping parts; whole-line navigation must still find the next complete grapheme.
            assert!(matches!(
                visual(&mut output, 0, false, true),
                crate::MarkdownComposedVisualAnswer::Found { utf16: 2, .. }
            ));
        } else {
            assert_eq!(output.paragraph_base_level(&line).unwrap(), 1);
        }
        output.close().unwrap();
    }
}

#[test]
fn trailing_whitespace_source_edit_rejects_old_line_and_keeps_snapshot_and_disk_distinct() {
    let text = "original   ";
    let mut fixture = Fixture::new(text);
    let mapping = fixture.source.original_backing_observer().unwrap();
    let old_source = fixture.source.resource_observer();
    let mut original = compose(&fixture);
    let line = original.last_line().unwrap().unwrap();
    let mut query = original.begin_line_trailing_whitespace(&line).unwrap();
    original
        .step_line_trailing_whitespace(&line, &mut query, ONE)
        .unwrap();
    // Publish a real canonical edit, then capture its new Source. The old output
    // remains a valid immutable snapshot, not an alias of the active document root.
    fixture
        .document
        .replace_utf16_range(0..8, "new", crate::MarkdownTextSelection::caret(3))
        .unwrap();
    fixture.source = MarkdownLayoutTextSource::capture(&fixture.document);
    let new_source = fixture.source.resource_observer();
    let mut current = compose(&fixture);
    let new_line = current.last_line().unwrap().unwrap();
    let current_resources = current.resource_observer();
    assert!(matches!(
        current.step_line_trailing_whitespace(&new_line, &mut query, ONE),
        Err(Error::SourceMismatch)
    ));
    assert!(!current_resources.snapshot().file_open);
    // The active document's new root does not rewrite frozen old output; Source mismatch cannot revoke another healthy owner.
    assert_eq!(trim(&mut original, &line).0, 8);
    let path = fixture._directory.path().join("真实行查找.md");
    assert_eq!(std::fs::read(path).unwrap(), text.as_bytes());
    assert!(fixture.document.is_dirty() && fixture.document.can_undo());
    let resources = original.resource_observer();
    original.close().unwrap();
    drop((current, fixture));
    assert!(old_source.payload_released() && new_source.payload_released());
    assert!(!resources.snapshot().file_open && resources.snapshot().source_payload_released);
    assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
    drop((query, line, new_line)); // Live queries/line handles are weak identities retaining neither text nor returned FDs.
}

#[test]
fn trailing_whitespace_foreign_output_and_cancelled_cached_answer_release_real_reader() {
    let fixture = Fixture::new("same   ");
    let mut original = compose(&fixture);
    let line = original.last_line().unwrap().unwrap();
    let mut query = original.begin_line_trailing_whitespace(&line).unwrap();
    let mut foreign = compose(&fixture);
    let foreign_line = foreign.last_line().unwrap().unwrap();
    let foreign_resources = foreign.resource_observer();
    assert!(matches!(
        foreign.step_line_trailing_whitespace(&foreign_line, &mut query, ONE),
        Err(Error::SourceMismatch)
    ));
    assert!(!foreign_resources.snapshot().file_open);
    assert_eq!(trim(&mut original, &line).0, 4);
    let mut completed = original.begin_line_trailing_whitespace(&line).unwrap();
    loop {
        if original
            .step_line_trailing_whitespace(&line, &mut completed, ONE)
            .unwrap()
            .answer
            .is_some()
        {
            break;
        }
    }
    let resources = original.resource_observer();
    let source = fixture.source.resource_observer();
    let mapping = fixture.source.original_backing_observer().unwrap();
    // Cancellation still applies to a previously completed query. A cached answer
    // cannot bypass original authority or retain its reader after real cancellation.
    fixture.cancellation.cancel();
    assert!(matches!(
        original.step_line_trailing_whitespace(&line, &mut completed, ONE),
        Err(Error::Cancelled)
    ));
    let actual = resources.snapshot();
    assert!(!actual.file_open && actual.mapped_bytes == 0 && !actual.cleanup_failed);
    drop((original, foreign, fixture));
    assert!(source.payload_released());
    assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
    drop((query, completed, line, foreign_line));
}
