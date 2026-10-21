//! The production worker emits multiple lines through real style responses.
//! These tests neither access nor manufacture Store directories.
use super::*;

pub(super) fn composer(
    fixture: &Fixture,
    width: i64,
    cancel: MarkdownLineComposeCancellation,
) -> (MarkdownLineComposer, MarkdownLineRegisteredFont) {
    // The actual palette owns the font; the original production constructor creates
    // its sole coordinator/writer over this fixture's captured Source.
    let catalog = MarkdownLineFontCatalog::new();
    let actual = catalog.register(&font()).unwrap();
    let worker = MarkdownLineComposer::new(
        fixture.source.clone(),
        &fixture.source.authority(),
        catalog,
        width,
        cancel,
    )
    .unwrap();
    (worker, actual)
}
pub(super) fn drive(
    worker: &mut MarkdownLineComposer,
    registered: &MarkdownLineRegisteredFont,
    stop_after_first: bool,
) -> usize {
    // Consume only public bounded answers and real response factories. Complete
    // lines must cover a continuous Source prefix before this driver may return.
    let mut next = 0;
    let mut lines = 0;
    for _ in 0..100_000 {
        let progress = worker.step(budget()).unwrap();
        assert!(
            progress.bytes <= 7
                && progress.scalars <= 3
                && progress.records <= 4
                && progress.metadata_units <= 2
                && progress.shapes <= 1
        );
        match progress.answer {
            MarkdownLineComposeAnswer::Yielded => {}
            #[cfg(all(
                feature = "s2-table-window-prototype",
                feature = "paged-shaping-frontier"
            ))]
            MarkdownLineComposeAnswer::NeedsTableRow(_) => {
                panic!("未启用表格流的原文字 oracle 不应产生 Grid 请求")
            }
            MarkdownLineComposeAnswer::NeedStyle(request) => {
                let range = request.source_range();
                worker
                    .provide_style(
                        request
                            .respond_registered(range, registered, style())
                            .unwrap(),
                    )
                    .unwrap();
            }
            MarkdownLineComposeAnswer::LineReady(facts) => {
                assert_eq!(facts.source_utf16.start, next);
                assert!(facts.source_utf16.end > next);
                next = facts.source_utf16.end;
                lines += 1;
                if stop_after_first {
                    return lines;
                }
            }
            MarkdownLineComposeAnswer::Complete => return lines,
            #[cfg(feature = "paged-shaping-frontier")]
            MarkdownLineComposeAnswer::NeedsFrontier(reason) => {
                panic!("原完整item对照不应启前沿: {reason:?}")
            }
            MarkdownLineComposeAnswer::NeedsTabPlacement(other) => {
                panic!("无Tab夹具不应产生{other:?}")
            }
            MarkdownLineComposeAnswer::NeedsResolution(other) => {
                panic!("有限safe夹具不能冒称完成: {other:?}")
            }
            MarkdownLineComposeAnswer::NeedsControl(control) => {
                panic!("有限无控制夹具不应产生: {control:?}")
            }
        }
    }
    panic!("有限组合worker没有完成或签出真实行");
}
#[test]
fn production_worker_composes_mixed_items_into_real_lines_and_unique_readonly_output() {
    // One actual font covers three scripts. Both narrow and wider lines cross real
    // script changes; verify continuous Source, complete glyphs, and actual release
    // without inspecting the forest or asserting lookup call counts.
    let text = "office αλφα слово office βητα текст office gamma конец ".repeat(17);
    for width in [140, 800] {
        let fixture = Fixture::new(&text);
        let source_mapping = fixture.source.original_backing_observer().unwrap();
        let before = fixture.document.generation();
        let (mut worker, registered) =
            composer(&fixture, width * 64, MarkdownLineComposeCancellation::new());
        let resources = worker.worker_resource_observer();
        let count = drive(&mut worker, &registered, false);
        assert!(count >= 3);
        let mut lines = worker.into_output().unwrap();
        let observer = lines.resource_observer();
        assert_eq!(lines.source_range(), 0..fixture.source.utf16_len());
        let mut line = lines.last_line().unwrap();
        let mut actual = 0;
        let mut end = fixture.source.utf16_len();
        let mut mixed = false;
        while let Some(current) = line {
            assert_eq!(current.facts().source_utf16.end, end);
            mixed |= current.facts().parts > 1;
            let mut covered = current.facts().source_utf16.start;
            for ordinal in 0..current.facts().parts {
                let part = lines.part(&current, ordinal).unwrap().unwrap();
                assert_eq!(part.facts().source_utf16.start, covered);
                assert!(
                    part.font_registration()
                        .lease()
                        .unwrap()
                        .font()
                        .permits(&registered.font().authority())
                );
                // Read all real output glyphs in small batches; empty reads or
                // .notdef cannot prove rendering. All three scripts are LTR, so
                // visual parts must also continuously cover their original line.
                let mut glyphs = [crate::MarkdownShapedGlyph {
                    glyph_id: 0,
                    source_utf16_cluster: 0,
                    flags: 0,
                    x_advance: 0,
                    y_advance: 0,
                    x_offset: 0,
                    y_offset: 0,
                }; 16];
                for start in (0..part.facts().glyphs).step_by(glyphs.len()) {
                    let end = (start + glyphs.len()).min(part.facts().glyphs);
                    let batch = &mut glyphs[..end - start];
                    lines.read_batch(&part, start..end, batch).unwrap();
                    assert!(batch.iter().all(|glyph| {
                        glyph.glyph_id != 0
                            && part
                                .facts()
                                .source_utf16
                                .contains(&glyph.source_utf16_cluster)
                    }));
                }
                covered = part.facts().source_utf16.end;
            }
            assert_eq!(covered, current.facts().source_utf16.end);
            end = current.facts().source_utf16.start;
            actual += 1;
            line = lines.previous_line(&current).unwrap();
        }
        assert_eq!((actual, end), (count, 0));
        assert!(mixed);
        assert_eq!(fixture.document.generation(), before);
        assert!(
            !fixture.document.is_dirty()
                && !fixture.document.can_undo()
                && !fixture.document.can_redo()
        );
        assert_eq!(
            std::fs::read(fixture._directory.path().join("真实分项.md")).unwrap(),
            text.as_bytes()
        );
        lines.close().unwrap();
        let closed = observer.snapshot();
        assert!(
            !closed.file_open
                && closed.mapped_bytes == 0
                && !closed.cleanup_failed
                && !closed.reader_failed
        );
        // Closing output does not release every Source owner. Drop the actual
        // registration/document before checking all workers and backing mappings.
        drop((registered, fixture));
        assert!(resources.is_released());
        assert!(source_mapping.unmap_succeeded());
        assert_eq!(source_mapping.mapped_bytes(), 0);
        assert!(!source_mapping.failed());
    }
}
#[test]
fn production_worker_can_seal_only_its_real_line_prefix_without_claiming_the_tail() {
    // Stop after one actual LineReady, then consume the worker's sealed output.
    // Its real reader must expose only that prefix, not the unprepared suffix.
    let fixture = Fixture::new("office office office office office office office office");
    let (mut worker, registered) =
        composer(&fixture, 80 * 64, MarkdownLineComposeCancellation::new());
    assert_eq!(drive(&mut worker, &registered, true), 1);
    let mut lines = worker.into_output().unwrap();
    assert!(lines.source_range().end < fixture.source.utf16_len());
    let line = lines.last_line().unwrap().unwrap();
    assert_eq!(line.facts().ordinal, 0);
    assert!(lines.previous_line(&line).unwrap().is_none());
    assert_eq!(lines.source_range(), line.facts().source_utf16);
    lines.close().unwrap();
}
#[test]
fn control_characters_are_explicit_pending_work_and_never_published_as_missing_glyphs() {
    // Exercise public control requests and exact CRLF/Unicode separator spans.
    // With no response supplied, neither text glyphs nor completed output may appear.
    for (text, offset, kind) in [
        ("a\tb", 1, MarkdownLineInlineControlKind::Tab),
        ("a\r\nb", 1, MarkdownLineInlineControlKind::CarriageReturn),
        ("a\u{85}b", 1, MarkdownLineInlineControlKind::NextLine),
        (
            "a\u{2028}b",
            1,
            MarkdownLineInlineControlKind::LineSeparator,
        ),
        (
            "a\u{2029}b",
            1,
            MarkdownLineInlineControlKind::ParagraphSeparator,
        ),
        ("\n", 0, MarkdownLineInlineControlKind::LineFeed),
    ] {
        let fixture = Fixture::new(text);
        let (mut worker, registered) =
            composer(&fixture, 800 * 64, MarkdownLineComposeCancellation::new());
        let mut seen = false;
        for _ in 0..10000 {
            match worker
                .step(MarkdownLineComposeBudget::default())
                .unwrap_or_else(|error| panic!("control {kind:?} 未进入明确控制阶段: {error:?}"))
                .answer
            {
                MarkdownLineComposeAnswer::NeedStyle(request) => {
                    let range = request.source_range();
                    worker
                        .provide_style(
                            request
                                .respond_registered(range, &registered, style())
                                .unwrap(),
                        )
                        .unwrap();
                }
                MarkdownLineComposeAnswer::NeedsControl(actual) => {
                    assert_eq!(
                        actual.source_utf16,
                        offset
                            ..offset
                                + if kind == MarkdownLineInlineControlKind::CarriageReturn {
                                    2
                                } else {
                                    1
                                }
                    );
                    assert_eq!(actual.kind, kind);
                    seen = true;
                    break;
                }
                MarkdownLineComposeAnswer::Yielded => {}
                _ => panic!("真实控制尚未处理时禁止Complete/LineReady/fallback"),
            }
        }
        assert!(seen);
        assert!(worker.into_output().is_err());
    }
}
#[test]
fn worker_cancel_and_zero_budget_revoke_the_whole_candidate_without_source_mutation() {
    // Cancel the actual worker and separately reject an invalid budget. Repeated
    // calls observe terminal revocation, while the original document stays unchanged.
    let fixture = Fixture::new("office office");
    let cancellation = MarkdownLineComposeCancellation::new();
    let (mut worker, _registered) = composer(&fixture, 80 * 64, cancellation.clone());
    cancellation.cancel();
    assert!(matches!(
        worker.step(budget()),
        Err(MarkdownLineComposeError::Cancelled)
    ));
    assert!(matches!(
        worker.step(budget()),
        Err(MarkdownLineComposeError::Poisoned)
    ));
    assert_eq!(fixture.document.generation(), 0);
    let (mut other, _) = composer(&fixture, 80 * 64, MarkdownLineComposeCancellation::new());
    let mut zero = budget();
    zero.records = 0;
    assert!(matches!(
        other.step(zero),
        Err(MarkdownLineComposeError::InvalidRequest)
    ));
    assert!(matches!(
        other.step(budget()),
        Err(MarkdownLineComposeError::Poisoned)
    ));
}

#[test]
fn production_worker_commits_signed_unsafe_prefixes_without_full_tail_reshaping() {
    // Actual ffi ligatures and a narrow width force original unsafe-prefix
    // selection/reshape/Store publication. Complete output, not a private phase,
    // must cover every canonical position and retire each real selector child.
    let fixture = Fixture::new(&"ffi".repeat(16));
    let (mut worker, registered) =
        composer(&fixture, 8 * 64, MarkdownLineComposeCancellation::new());
    let all_resources = worker.worker_resource_observer();
    let mut count = 0;
    let mut cursor = 0;
    let mut reshapes = 0;
    let mut reshaped_bytes = 0;
    for _ in 0..100_000 {
        let progress = worker.step(budget()).unwrap();
        assert!(progress.bytes <= 7 && progress.records <= 4 && progress.metadata_units <= 2);
        reshapes += progress.shapes;
        reshaped_bytes += progress.bytes;
        match progress.answer {
            MarkdownLineComposeAnswer::Yielded => {}
            MarkdownLineComposeAnswer::NeedStyle(request) => {
                let range = request.source_range();
                worker
                    .provide_style(
                        request
                            .respond_registered(range, &registered, style())
                            .unwrap(),
                    )
                    .unwrap();
            }
            MarkdownLineComposeAnswer::LineReady(facts) => {
                assert_eq!(facts.source_utf16.start, cursor);
                assert!(facts.source_utf16.end > cursor && facts.glyphs > 0);
                if facts.kind == MarkdownComposedLineKind::Mandatory {
                    // Only actual final EOF retains Mandatory. An intermediate
                    // glyph part cannot borrow that fact to claim document completion.
                    assert_eq!(facts.source_utf16.end, fixture.source.utf16_len());
                } else {
                    assert!(matches!(
                        facts.kind,
                        MarkdownComposedLineKind::Emergency
                            | MarkdownComposedLineKind::Overflow
                            | MarkdownComposedLineKind::Allowed
                    ));
                }
                cursor = facts.source_utf16.end;
                count += 1;
            }
            MarkdownLineComposeAnswer::Complete => break,
            _ => panic!("原Stored签名prefix必须真实消费，不能永久停在接口已存在的Pending"),
        }
    }
    assert_eq!(cursor, fixture.source.utf16_len());
    assert!(count > 16 && reshapes > 16);
    // Public work includes original item input and coordination metadata reads.
    // Bound repeated whole-suffix work only for this finite regression fixture.
    assert!(reshaped_bytes < 16 * fixture.source.byte_len());
    let mut output = worker.into_output().unwrap();
    let observer = output.resource_observer();
    let mut line = output.last_line().unwrap();
    let mut visited = 0;
    while let Some(current) = line {
        let part = output.part(&current, 0).unwrap().unwrap();
        assert_eq!(part.facts().source_utf16, current.facts().source_utf16);
        let mut glyphs = vec![
            crate::MarkdownShapedGlyph {
                glyph_id: 0,
                source_utf16_cluster: 0,
                flags: 0,
                x_advance: 0,
                y_advance: 0,
                x_offset: 0,
                y_offset: 0
            };
            part.facts().glyphs
        ];
        output
            .read_batch(&part, 0..glyphs.len(), &mut glyphs)
            .unwrap();
        assert!(glyphs.iter().all(|glyph| {
            part.facts()
                .source_utf16
                .contains(&glyph.source_utf16_cluster)
        }));
        assert!(matches!(
            output
                .cluster(&part, part.facts().source_utf16.start)
                .unwrap(),
            crate::MarkdownGlyphClusterAnswer::Known(Some(_))
        ));
        visited += 1;
        line = output.previous_line(&current).unwrap();
    }
    assert_eq!(visited, count);
    output.close().unwrap();
    let closed = observer.snapshot();
    assert!(
        !closed.file_open
            && closed.mapped_bytes == 0
            && !closed.cleanup_failed
            && !closed.reader_failed
    );
    // The actual selector pipeline must release all real prefix children and
    // temporary cluster resources, without retaining a receipt owner per line.
    let checked = all_resources.snapshot().unwrap();
    assert!(checked.resolvers.registered > 0);
    assert!(checked.resolvers.jobs_created > 16);
    assert_eq!(checked.resolvers.active, 0);
    assert_eq!(
        checked.resolvers.jobs_created,
        checked.resolvers.jobs_released
    );
    assert!(checked.clusters.registered > 16);
    assert_eq!(checked.clusters.active, 0);
    drop((registered, fixture));
    assert!(all_resources.is_released());
}

fn paragraph() -> MarkdownLineParagraphStyle {
    MarkdownLineParagraphStyle {
        tab_stops: vec![MarkdownLineTabStop {
            location_26_6: 30 * 64,
            alignment: MarkdownLineTabAlignment::Left,
        }],
        default_tab_interval_26_6: Some(40 * 64),
    }
}
fn drive_controls(
    worker: &mut MarkdownLineComposer,
    font: &MarkdownLineRegisteredFont,
) -> Vec<MarkdownComposedLineFacts> {
    // Finite control fixtures use their real response factories, including actual
    // paragraph tab style; only public committed LineReady facts are retained here.
    let mut lines = Vec::new();
    for _ in 0..100_000 {
        match worker
            .step(MarkdownLineComposeBudget::default())
            .unwrap()
            .answer
        {
            MarkdownLineComposeAnswer::Yielded => {}
            #[cfg(all(
                feature = "s2-table-window-prototype",
                feature = "paged-shaping-frontier"
            ))]
            MarkdownLineComposeAnswer::NeedsTableRow(_) => {
                panic!("未启用表格流的原文字 oracle 不应产生 Grid 请求")
            }
            MarkdownLineComposeAnswer::NeedStyle(request) => {
                let range = request.source_range();
                worker
                    .provide_style(request.respond_registered(range, font, style()).unwrap())
                    .unwrap();
            }
            MarkdownLineComposeAnswer::NeedsControl(request) => worker
                .provide_control(request.respond(font, 16.0, paragraph()).unwrap())
                .unwrap(),
            MarkdownLineComposeAnswer::LineReady(facts) => lines.push(facts),
            MarkdownLineComposeAnswer::Complete => return lines,
            MarkdownLineComposeAnswer::NeedsResolution(reason) => {
                panic!("控制夹具不应进入不相关解析:{reason:?}")
            }
            #[cfg(feature = "paged-shaping-frontier")]
            MarkdownLineComposeAnswer::NeedsFrontier(reason) => {
                panic!("原控制对照不应启前沿: {reason:?}")
            }
            MarkdownLineComposeAnswer::NeedsTabPlacement(reason) => {
                panic!("明确Left停靠不应待后继:{reason:?}")
            }
        }
    }
    panic!("有限控制夹具没有真实完成");
}
#[test]
fn physical_breaks_empty_lines_and_trailing_line_have_real_non_glyph_parts() {
    // These independent expected physical-line counts include real empty EOF
    // paragraphs. Inspect their actual zero-glyph parts rather than infer them from
    // text length or add a drawable space for a separator.
    for (text, count) in [
        ("", 1),
        ("\n", 2),
        ("a\r\nb", 2),
        ("a\n\n", 3),
        ("ab\u{2029}", 2),
        ("ab\u{85}cd", 2),
        ("a\u{2028}", 2),
    ] {
        let fixture = Fixture::new(text);
        let (mut worker, font) =
            composer(&fixture, 800 * 64, MarkdownLineComposeCancellation::new());
        let facts = drive_controls(&mut worker, &font);
        assert_eq!(facts.len(), count, "{text:?}");
        let mut next = 0;
        for line in &facts {
            assert_eq!(line.source_utf16.start, next);
            next = line.source_utf16.end;
        }
        assert_eq!(next, fixture.source.utf16_len());
        let mut output = worker.into_output().unwrap();
        let observer = output.resource_observer();
        let mut line = output.last_line().unwrap();
        let mut actual = 0;
        let mut break_parts = 0;
        while let Some(value) = line {
            for ordinal in 0..value.facts().parts {
                let part = output.part(&value, ordinal).unwrap().unwrap();
                output.read_batch(&part, 0..0, &mut []).unwrap();
                if part.facts().kind == MarkdownComposedLinePartKind::ParagraphBreak {
                    break_parts += 1;
                    assert_eq!(part.facts().glyphs, 0);
                    assert_eq!(part.facts().x_advance_26_6, 0);
                    assert!(
                        part.font_registration()
                            .lease()
                            .unwrap()
                            .font()
                            .permits(&font.font().authority())
                    );
                    if !part.facts().source_utf16.is_empty() {
                        let crate::MarkdownGlyphClusterAnswer::Known(Some(cluster)) = output
                            .cluster(&part, part.facts().source_utf16.start)
                            .unwrap()
                        else {
                            panic!("真实控制源不可消失");
                        };
                        assert_eq!(cluster.glyphs, 0..0);
                        assert_eq!(cluster.source_utf16, part.facts().source_utf16);
                    }
                }
            }
            actual += 1;
            line = output.previous_line(&value).unwrap();
        }
        assert_eq!(actual, count);
        assert!(break_parts > 0);
        output.close().unwrap();
        assert!(!observer.snapshot().file_open && !observer.snapshot().cleanup_failed);
    }
}
#[test]
fn left_tabs_recompute_from_each_actual_line_pen_and_never_draw_space_glyphs() {
    // The original explicit stop/default interval determines each tab's actual
    // advance from this line's pen. Wrapped lines cannot retain a previous line's pen.
    let fixture = Fixture::new("aa aa\tbb cc dd\tend");
    let (mut worker, font) = composer(&fixture, 55 * 64, MarkdownLineComposeCancellation::new());
    let facts = drive_controls(&mut worker, &font);
    assert!(facts.len() >= 2);
    let mut output = worker.into_output().unwrap();
    let mut line = output.last_line().unwrap();
    let mut tabs = 0;
    while let Some(value) = line {
        let mut pen = 0;
        for ordinal in 0..value.facts().parts {
            let part = output.part(&value, ordinal).unwrap().unwrap();
            let facts = part.facts();
            assert_eq!(facts.origin_x_26_6, pen);
            if facts.kind == MarkdownComposedLinePartKind::Tab {
                tabs += 1;
                assert_eq!(facts.glyphs, 0);
                let expected = if pen < 30 * 64 {
                    30 * 64
                } else {
                    30 * 64 + ((pen - 30 * 64).div_euclid(40 * 64) + 1) * 40 * 64
                };
                assert_eq!(facts.x_advance_26_6, expected - pen);
                let crate::MarkdownGlyphClusterAnswer::Known(Some(cluster)) =
                    output.cluster(&part, facts.source_utf16.start).unwrap()
                else {
                    panic!("实际Tab范围缺失");
                };
                assert_eq!(cluster.x_advance_26_6, facts.x_advance_26_6);
                assert_eq!(cluster.glyphs, 0..0);
            }
            pen += facts.x_advance_26_6;
        }
        assert_eq!(pen, value.facts().x_advance_26_6);
        line = output.previous_line(&value).unwrap();
    }
    assert_eq!(tabs, 2);
    output.close().unwrap();
}
#[test]
fn right_tab_stays_typed_pending_until_actual_lookahead_and_default_interval_are_known() {
    // Submit a real right-aligned stop without lookahead. The unresolved typed
    // placement must remain visible instead of substituting left alignment or space.
    let fixture = Fixture::new("a\tb");
    let (mut worker, font) = composer(&fixture, 800 * 64, MarkdownLineComposeCancellation::new());
    let mut seen = false;
    for _ in 0..10000 {
        match worker
            .step(MarkdownLineComposeBudget::default())
            .unwrap()
            .answer
        {
            MarkdownLineComposeAnswer::Yielded => {}
            MarkdownLineComposeAnswer::NeedStyle(request) => {
                let range = request.source_range();
                worker
                    .provide_style(request.respond_registered(range, &font, style()).unwrap())
                    .unwrap();
            }
            MarkdownLineComposeAnswer::NeedsControl(request) => {
                worker
                    .provide_control(
                        request
                            .respond(
                                &font,
                                16.0,
                                MarkdownLineParagraphStyle {
                                    tab_stops: vec![MarkdownLineTabStop {
                                        location_26_6: 30 * 64,
                                        alignment: MarkdownLineTabAlignment::Right,
                                    }],
                                    default_tab_interval_26_6: None,
                                },
                            )
                            .unwrap(),
                    )
                    .unwrap();
            }
            MarkdownLineComposeAnswer::NeedsTabPlacement(
                MarkdownLineTabPlacement::NeedsLookahead { stop, .. },
            ) => {
                assert_eq!(stop.alignment, MarkdownLineTabAlignment::Right);
                seen = true;
                break;
            }
            _ => panic!("未测后继时禁止Tab/LineReady/Complete"),
        }
    }
    assert!(seen);
    assert!(worker.into_output().is_err());
}

fn next_control(
    worker: &mut MarkdownLineComposer,
    font: &MarkdownLineRegisteredFont,
) -> MarkdownLineInlineControl {
    for _ in 0..10000 {
        match worker
            .step(MarkdownLineComposeBudget::default())
            .unwrap()
            .answer
        {
            MarkdownLineComposeAnswer::Yielded => {}
            MarkdownLineComposeAnswer::NeedStyle(request) => {
                let range = request.source_range();
                worker
                    .provide_style(request.respond_registered(range, font, style()).unwrap())
                    .unwrap();
            }
            MarkdownLineComposeAnswer::NeedsControl(request) => return request,
            _ => panic!("有限控制请求不能被提前发布替代"),
        }
    }
    panic!("有限控制没有待决请求");
}
#[test]
fn equal_source_control_nonce_is_not_transferable_and_replay_never_commits_data() {
    // Separate real workers on one Source issue distinct control nonces. Neither a
    // foreign response nor a consumed replay can publish control or mutate the file.
    let fixture = Fixture::new("a\tb");
    let (mut first, font) = composer(&fixture, 800 * 64, MarkdownLineComposeCancellation::new());
    let (mut second, other_font) =
        composer(&fixture, 800 * 64, MarkdownLineComposeCancellation::new());
    let request = next_control(&mut first, &font);
    let other = next_control(&mut second, &other_font);
    assert_eq!(request.source_utf16, other.source_utf16);
    assert!(request.source() == other.source());
    let foreign = request.clone().respond(&font, 16.0, paragraph()).unwrap();
    assert!(matches!(
        second.provide_control(foreign),
        Err(MarkdownLineComposeError::SourceMismatch)
    ));
    let replay = request.clone().respond(&font, 16.0, paragraph()).unwrap();
    first
        .provide_control(request.respond(&font, 16.0, paragraph()).unwrap())
        .unwrap();
    assert!(matches!(
        first.provide_control(replay),
        Err(MarkdownLineComposeError::InvalidRequest)
    ));
    assert_eq!(fixture.document.generation(), 0);
    assert_eq!(fixture.document.utf16_len(), 3);
}
#[test]
fn source_failure_precedes_control_response_and_cannot_be_hidden_by_cached_nonce_or_font() {
    let fixture = Fixture::new("a\tb");
    let (mut worker, font) = composer(&fixture, 800 * 64, MarkdownLineComposeCancellation::new());
    let resources = worker.worker_resource_observer();
    let request = next_control(&mut worker, &font);
    let response = request.respond(&font, 16.0, paragraph()).unwrap();
    fixture
        .document
        .text_snapshot()
        .visit_byte_range(0..1, |_| Ok(()))
        .unwrap();
    // Existing EIO injection goes through actual mapped backing reclaim. It is not
    // a naturally induced kernel error; cached nonce/font data must still retain it.
    fixture.document.fail_next_original_reclaim_for_test();
    assert_eq!(
        fixture.document.reclaim_original_read_windows_for_test(),
        Err(crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO))
    );
    assert!(matches!(
        worker.provide_control(response),
        Err(MarkdownLineComposeError::Source(
            crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
    assert!(matches!(
        worker.step(budget()),
        Err(MarkdownLineComposeError::Source(
            crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
    let closed = resources.snapshot().unwrap();
    assert_eq!(closed.first_source_errno, Some(libc::EIO));
    assert!(!closed.writer.unwrap().file_open);
    assert!(!closed.scripts.unwrap().run_file_open);
    assert!(!closed.graphemes.unwrap().metadata_file_open);
    assert!(!closed.line_breaks.unwrap().metadata_file_open);
    assert_eq!(closed.shapes.active, 0);
    assert_eq!(closed.clusters.active, 0);
    drop((worker, font, fixture));
    assert!(resources.snapshot().unwrap().source_payload_released);
    assert!(!resources.is_released()); // Real FD release cannot erase the original EIO.
}
