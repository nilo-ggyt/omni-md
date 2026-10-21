//! Actual shaping and continuation of original natural item-end GCBs, without Native substitutes,
//! artificial stages, or another product body representation.
use super::*;
use crate::{
    MarkdownLineComposeBudget, MarkdownLineComposeCancellation, MarkdownLineComposeError,
    MarkdownLineComposeStyle, MarkdownLineFontProbe, MarkdownLineItem, MarkdownLineItemAnswer,
    MarkdownLineItemCoordinator, MarkdownLineItemFacts, MarkdownLineStyleRequest,
};

fn style() -> MarkdownLineComposeStyle {
    MarkdownLineComposeStyle {
        presentation: Default::default(),
        points: 16.0,
        language: Some("en".into()),
        features: Vec::new(),
    }
}

fn budget() -> MarkdownLineComposeBudget {
    MarkdownLineComposeBudget {
        bytes: 7,
        scalars: 3,
        records: 3,
        metadata_units: 2,
    }
}

fn advance(worker: &mut MarkdownLineItemCoordinator) -> MarkdownLineItemAnswer {
    let limit = budget();
    let step = worker.step(limit).unwrap();
    assert!(step.bytes <= limit.bytes && step.scalars <= limit.scalars);
    assert!(step.records <= limit.records && step.metadata_units <= limit.metadata_units);
    assert!(step.shapes <= 1);
    step.answer
}

fn glyphs(mut item: MarkdownLineItem) -> (MarkdownLineItemFacts, Vec<crate::MarkdownShapedGlyph>) {
    let facts = item.facts().clone();
    let mut result = Vec::new(); // Finite selected-item oracle records, never a product owner or cache.
    while let Some(glyph) = item.glyph(result.len()).unwrap() {
        result.push(glyph);
    }
    (facts, result)
}

fn until_terminal(
    fixture: &Fixture,
    worker: &mut MarkdownLineItemCoordinator,
) -> (
    MarkdownLineStyleRequest,
    Vec<(MarkdownLineItemFacts, Vec<crate::MarkdownShapedGlyph>)>,
) {
    // Advance the actual coordinator until it requests the blocking GCB's font trial. Ordinary
    // prefix text must not create a per-character Native style-selection loop.
    let mut records = Vec::new();
    let mut initial = false;
    for _ in 0..100_000 {
        match advance(worker) {
            MarkdownLineItemAnswer::Yielded => {}
            MarkdownLineItemAnswer::NeedStyle(request) => {
                if request.continuation_font().is_some() {
                    assert!(fixture.font.permits(request.continuation_font().unwrap()));
                    assert!(initial);
                    return (request, records);
                }
                assert!(!initial, "普通中文/字素不能逐个生成Native字体试次");
                initial = true;
                let range = request.source_range();
                worker
                    .provide_style(
                        request
                            .respond(range, fixture.font.clone(), style())
                            .unwrap(),
                    )
                    .unwrap();
            }
            MarkdownLineItemAnswer::ItemReady(_) => {
                records.push(glyphs(worker.take_item().unwrap()))
            }
            _ => panic!("真实自然末GCB之前不应拒绝、出现控制或假EOF"),
        }
    }
    panic!("有限真实协调器没有交付末GCB请求");
}

fn trial(
    fixture: &Fixture,
    probe: &MarkdownLineFontProbe,
    font: &MarkdownLayoutFont,
) -> Vec<crate::MarkdownShapedGlyph> {
    let mut job = MarkdownShapingJob::new(
        fixture.source.clone(),
        &fixture.authority,
        font.clone(),
        &font.authority(),
        probe.source_range(),
        probe.shaping_options(&style()).unwrap(),
    )
    .unwrap();
    while job.status() == MarkdownShapingStatus::Ingesting {
        let step = job
            .step(
                &fixture.authority,
                &font.authority(),
                MarkdownShapingBudget {
                    bytes: 1,
                    scalars: 1,
                },
            )
            .unwrap();
        assert!(step.bytes <= 1 && step.scalars <= 1);
    }
    job.shape(&fixture.authority, &font.authority()).unwrap();
    let mut result = Vec::new(); // Inspect complete actual GCB output, not nominal cmap or endpoint samples, for missing glyphs.
    for index in 0..job.profile().unwrap().glyph_count {
        result.push(
            job.glyph(&fixture.authority, &font.authority(), index)
                .unwrap()
                .unwrap(),
        );
    }
    let observer = job.resource_observer();
    drop(job);
    let released = observer.snapshot();
    assert_eq!(
        (
            released.files_open,
            released.mappings_open,
            released.mapped_bytes
        ),
        (0, 0, 0)
    );
    assert!(!released.cleanup_failed && released.first_error.is_none());
    result
}

fn finish_through(
    worker: &mut MarkdownLineItemCoordinator,
    through: usize,
    records: &mut Vec<(MarkdownLineItemFacts, Vec<crate::MarkdownShapedGlyph>)>,
) {
    for _ in 0..100_000 {
        match advance(worker) {
            MarkdownLineItemAnswer::Yielded => {}
            MarkdownLineItemAnswer::ItemReady(_) => {
                let record = glyphs(worker.take_item().unwrap());
                let end = record.0.source_utf16.end;
                records.push(record);
                if end == through {
                    return;
                }
                assert!(end < through);
            }
            _ => panic!("接受末GCB之后不能重复请求样式或发布假EOF"),
        }
    }
    panic!("有限末GCB没有真正完成");
}

fn oracle(
    fixture: &Fixture,
    font: &MarkdownLayoutFont,
    range: Range<usize>,
) -> Vec<crate::MarkdownShapedGlyph> {
    let mut options = fixture.options();
    options.beginning_of_text = range.start == 0;
    options.end_of_text = false; // Following Han and actual CRLF remain; a font boundary is not paragraph end.
    let mut job = MarkdownShapingJob::new(
        fixture.source.clone(),
        &fixture.authority,
        font.clone(),
        &font.authority(),
        range,
        options,
    )
    .unwrap();
    while job.status() == MarkdownShapingStatus::Ingesting {
        job.step(
            &fixture.authority,
            &font.authority(),
            MarkdownShapingBudget::default(),
        )
        .unwrap();
    }
    job.shape(&fixture.authority, &font.authority()).unwrap();
    (0..job.profile().unwrap().glyph_count)
        .map(|index| {
            job.glyph(&fixture.authority, &font.authority(), index)
                .unwrap()
                .unwrap()
        })
        .collect()
}

#[test]
fn terminal_same_font_keeps_original_plan_and_all_glyph_fields_without_a_font_boundary() {
    // Actual CRLF closes original SCX; following Han makes the euro sign a natural Latn item end,
    // not an artificially shortened style window.
    let fixture = Fixture::new(&format!("{}€中\r\n", "é".repeat(462)), independent_font());
    let source = fixture.source.resource_observer();
    let backing = fixture.document.original_backing_observer().unwrap();
    let font = fixture.font.resource_observer();
    let mut worker = MarkdownLineItemCoordinator::new_with_frontier(
        fixture.source.clone(),
        &fixture.authority,
        MarkdownLineComposeCancellation::new(),
    )
    .unwrap();
    let (request, mut records) = until_terminal(&fixture, &mut worker);
    assert_eq!(request.source_range(), 462..463);
    let probe = worker.font_probe(&request).unwrap();
    let actual = trial(&fixture, &probe, &fixture.font);
    assert_eq!(actual.len(), 1);
    assert!(actual.iter().all(|glyph| glyph.glyph_id != 0));
    let range = request.source_range();
    worker
        .provide_style(
            request
                .respond(range, fixture.font.clone(), style())
                .unwrap(),
        )
        .unwrap();
    assert!(matches!(
        probe.validate(),
        Err(MarkdownLineComposeError::Poisoned)
    ));
    finish_through(&mut worker, 463, &mut records);
    assert_eq!(
        records
            .iter()
            .map(|record| record.0.source_utf16.clone())
            .collect::<Vec<_>>(),
        vec![0..256, 256..463]
    );
    let all = records
        .iter()
        .flat_map(|record| record.1.iter().copied())
        .collect::<Vec<_>>();
    assert_eq!(all, oracle(&fixture, &fixture.font, 0..463));
    assert!(records.iter().all(|record| !record.0.end_of_text));
    drop((worker, probe, fixture));
    assert!(source.payload_released() && font.payload_released());
    assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
}

#[cfg(target_os = "macos")]
#[test]
fn terminal_actual_missing_font_preserves_206_scalars_before_the_real_emoji_font_run() {
    let fixture = Fixture::new(&format!("{}😀中\r\n", "é".repeat(462)), independent_font());
    let source = fixture.source.resource_observer();
    let backing = fixture.document.original_backing_observer().unwrap();
    let mut worker = MarkdownLineItemCoordinator::new_with_frontier(
        fixture.source.clone(),
        &fixture.authority,
        MarkdownLineComposeCancellation::new(),
    )
    .unwrap();
    let (request, mut records) = until_terminal(&fixture, &mut worker);
    assert_eq!(request.source_range(), 462..464);
    let probe = worker.font_probe(&request).unwrap();
    assert!(
        trial(&fixture, &probe, &fixture.font)
            .iter()
            .any(|glyph| glyph.glyph_id == 0)
    );
    // Shape this machine's actual system font in Core. It proves only this finite GCB; its name
    // grants no production-plan capability.
    let bytes = crate::MarkdownLayoutFontBytes::capture_file(
        std::path::Path::new("/System/Library/Fonts/Apple Color Emoji.ttc"),
        512 * 1024 * 1024,
    )
    .unwrap();
    let mapped = bytes.resource_observer();
    let name = read_fonts::FontRef::from_index(bytes.as_ref(), 0)
        .unwrap()
        .table_data(read_fonts::types::Tag::new(b"name"))
        .unwrap()
        .as_bytes()
        .to_vec();
    let emoji =
        MarkdownLayoutFont::resolve_name_table_at_size(bytes.blob(), &name, &[], 16.0).unwrap();
    drop(bytes);
    let expected_emoji = trial(&fixture, &probe, &emoji);
    assert!(!expected_emoji.is_empty() && expected_emoji.iter().all(|glyph| glyph.glyph_id != 0));
    let range = request.source_range();
    worker
        .provide_style(request.respond(range, emoji.clone(), style()).unwrap())
        .unwrap();
    assert!(matches!(
        probe.validate(),
        Err(MarkdownLineComposeError::Poisoned)
    ));
    finish_through(&mut worker, 464, &mut records);
    assert_eq!(
        records
            .iter()
            .map(|record| record.0.source_utf16.clone())
            .collect::<Vec<_>>(),
        vec![0..256, 256..462, 462..464]
    );
    let chinese_residual = records[..2]
        .iter()
        .flat_map(|record| record.1.iter().copied())
        .collect::<Vec<_>>();
    assert_eq!(chinese_residual, oracle(&fixture, &fixture.font, 0..462));
    assert_eq!(records[1].1.len(), 206);
    assert_eq!(records[2].1, expected_emoji);
    assert!(records.iter().all(|record| !record.0.end_of_text));
    drop((worker, probe, fixture, emoji));
    assert!(source.payload_released());
    assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
    assert!(
        mapped.mapping_drop_completed()
            && mapped.unmap_succeeded()
            && mapped.mapped_bytes() == 0
            && !mapped.failed()
    );
}

#[test]
fn terminal_cancel_and_late_source_error_cannot_resume_the_paused_owner() {
    // Pause at a genuine terminal style request, then request cancellation with or without a real
    // source reclaim failure. Neither a saved response nor probe may resume the ended owner.
    for io in [false, true] {
        let fixture = Fixture::new(&format!("{}€中\r\n", "é".repeat(462)), independent_font());
        let cancel = MarkdownLineComposeCancellation::new();
        let mut worker = MarkdownLineItemCoordinator::new_with_frontier(
            fixture.source.clone(),
            &fixture.authority,
            cancel.clone(),
        )
        .unwrap();
        let (request, _) = until_terminal(&fixture, &mut worker);
        let probe = worker.font_probe(&request).unwrap();
        let source = fixture.source.resource_observer();
        let backing = fixture.document.original_backing_observer().unwrap();
        if io {
            fixture.source.visit_byte_range(0..1, |_| Ok(())).unwrap();
            fixture.document.fail_next_original_reclaim_for_test();
            assert!(matches!(
                fixture.document.reclaim_original_read_windows_for_test(),
                Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                    libc::EIO
                ))
            ));
        }
        cancel.cancel();
        let range = request.source_range();
        let result = worker.provide_style(
            request
                .respond(range, fixture.font.clone(), style())
                .unwrap(),
        );
        if io {
            assert!(matches!(
                result,
                Err(MarkdownLineComposeError::Source(
                    MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
                ))
            ));
        } else {
            assert!(matches!(result, Err(MarkdownLineComposeError::Cancelled)));
        }
        assert!(probe.validate().is_err());
        drop((worker, probe, fixture));
        assert!(source.payload_released());
        assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0);
        assert_eq!(backing.first_errno(), io.then_some(libc::EIO));
    }
}

#[test]
fn terminal_consumed_style_nonce_cannot_be_replayed_into_the_same_source_owner() {
    // Clone only the existing request to exercise replay after its actual response is consumed.
    // Equal source/range/font values must not recreate the retired style nonce.
    let fixture = Fixture::new(&format!("{}€中\r\n", "é".repeat(462)), independent_font());
    let source = fixture.source.resource_observer();
    let backing = fixture.document.original_backing_observer().unwrap();
    let mut worker = MarkdownLineItemCoordinator::new_with_frontier(
        fixture.source.clone(),
        &fixture.authority,
        MarkdownLineComposeCancellation::new(),
    )
    .unwrap();
    let (request, _) = until_terminal(&fixture, &mut worker);
    let old = request.clone();
    let probe = worker.font_probe(&request).unwrap();
    assert!(
        trial(&fixture, &probe, &fixture.font)
            .iter()
            .all(|glyph| glyph.glyph_id != 0)
    );
    let range = request.source_range();
    worker
        .provide_style(
            request
                .respond(range.clone(), fixture.font.clone(), style())
                .unwrap(),
        )
        .unwrap();
    assert!(matches!(
        probe.validate(),
        Err(MarkdownLineComposeError::Poisoned)
    ));
    let replay = worker.provide_style(old.respond(range, fixture.font.clone(), style()).unwrap());
    assert!(
        matches!(&replay, Err(MarkdownLineComposeError::InvalidRequest)),
        "响应已消费后没有当前Style请求，重放必须被原阶段合同拒绝：{replay:?}",
    );
    assert!(matches!(
        worker.step(budget()),
        Err(MarkdownLineComposeError::Poisoned)
    ));
    drop((worker, probe, fixture));
    assert!(source.payload_released());
    assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
}

#[test]
fn terminal_same_font_continues_through_original_composer_store_and_real_line_end() {
    use crate::{
        MarkdownLineComposeAnswer, MarkdownLineComposer, MarkdownLineFontCatalog,
        MarkdownLineParagraphStyle,
    };
    let fixture = Fixture::new(&format!("{}€\r\n", "é".repeat(462)), independent_font());
    let source = fixture.source.resource_observer();
    let backing = fixture.document.original_backing_observer().unwrap();
    let catalog = MarkdownLineFontCatalog::new();
    let registered = catalog.register(&fixture.font).unwrap();
    let mut worker = MarkdownLineComposer::new_with_frontier(
        fixture.source.clone(),
        &fixture.authority,
        catalog,
        100_000 * 64,
        MarkdownLineComposeCancellation::new(),
    )
    .unwrap();
    let mut styles = 0;
    let mut complete = false;
    for _ in 0..100_000 {
        let step = worker.step(budget()).unwrap();
        assert!(step.bytes <= budget().bytes && step.scalars <= budget().scalars);
        assert!(step.metadata_units <= budget().metadata_units && step.shapes <= 1);
        match step.answer {
            #[cfg(all(
                feature = "s2-table-window-prototype",
                feature = "paged-shaping-frontier"
            ))]
            MarkdownLineComposeAnswer::NeedsTableRow(_) => {
                panic!("未启用表格流的原文字 oracle 不应产生 Grid 请求")
            }
            MarkdownLineComposeAnswer::Yielded | MarkdownLineComposeAnswer::LineReady(_) => {}
            MarkdownLineComposeAnswer::NeedStyle(request) => {
                styles += 1;
                let probe = worker.font_probe(&request).unwrap();
                if request.continuation_font().is_some() {
                    assert_eq!(request.source_range(), 462..463);
                    assert!(
                        trial(&fixture, &probe, &fixture.font)
                            .iter()
                            .all(|glyph| glyph.glyph_id != 0)
                    );
                }
                let range = request.source_range();
                worker
                    .provide_style(
                        request
                            .respond_registered(range, &registered, style())
                            .unwrap(),
                    )
                    .unwrap();
                assert!(matches!(
                    probe.validate(),
                    Err(MarkdownLineComposeError::Poisoned)
                ));
            }
            MarkdownLineComposeAnswer::NeedsControl(request) => {
                worker
                    .provide_control(
                        request
                            .respond(
                                &registered,
                                16.0,
                                MarkdownLineParagraphStyle {
                                    tab_stops: Vec::new(),
                                    default_tab_interval_26_6: None,
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
            MarkdownLineComposeAnswer::NeedsFrontier(reason) => {
                panic!("真实末GCB仍拒绝：{reason:?}")
            }
            MarkdownLineComposeAnswer::NeedsResolution(reason) => {
                panic!("原Store/Measure没有完成：{reason:?}")
            }
            MarkdownLineComposeAnswer::NeedsTabPlacement(_) => panic!("无Tab样本不能请求Tab"),
        }
    }
    assert!(complete);
    assert_eq!(styles, 2, "普通前缀只用一次原样式，另外一次仅对应真实末GCB");
    let mut output = worker.into_output().unwrap();
    let reader = output.resource_observer();
    let mut line = output.last_line().unwrap();
    let mut actual = Vec::new(); // Finite output oracle for this 463-glyph input, compared by original global clusters.
    while let Some(current) = line {
        for ordinal in 0..current.facts().parts {
            let part = output.part(&current, ordinal).unwrap().unwrap();
            for glyph in 0..part.facts().glyphs {
                let mut one = [crate::MarkdownShapedGlyph {
                    glyph_id: 0,
                    source_utf16_cluster: 0,
                    flags: 0,
                    x_advance: 0,
                    y_advance: 0,
                    x_offset: 0,
                    y_offset: 0,
                }];
                output
                    .read_batch(&part, glyph..glyph + 1, &mut one)
                    .unwrap();
                actual.push(one[0]);
            }
        }
        line = output.previous_line(&current).unwrap();
    }
    actual.sort_by_key(|glyph| glyph.source_utf16_cluster);
    assert_eq!(actual, oracle(&fixture, &fixture.font, 0..463));
    output.close().unwrap();
    let closed = reader.snapshot();
    assert!(
        !closed.file_open
            && closed.mapped_bytes == 0
            && !closed.cleanup_failed
            && !closed.reader_failed
    );
    drop((registered, fixture));
    assert!(source.payload_released());
    assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
}

#[test]
fn ordinary_shortened_style_and_nonterminal_symbol_do_not_gain_terminal_permission() {
    for shortened in [false, true] {
        let text = if shortened {
            format!("{}€{}中\r\n", "é".repeat(462), "é".repeat(10))
        } else {
            format!("{}€{}中\r\n", "é".repeat(462), "é".repeat(100))
        };
        let fixture = Fixture::new(&text, independent_font());
        let mut worker = MarkdownLineItemCoordinator::new_with_frontier(
            fixture.source.clone(),
            &fixture.authority,
            MarkdownLineComposeCancellation::new(),
        )
        .unwrap();
        let mut answered = false;
        let mut rejected = false;
        for _ in 0..100_000 {
            match advance(&mut worker) {
                MarkdownLineItemAnswer::Yielded => {}
                MarkdownLineItemAnswer::NeedStyle(request) => {
                    if request.continuation_font().is_some() {
                        assert!(!shortened, "缩短style不得取得真实SCX交接资格");
                        let probe = worker.font_probe(&request).unwrap();
                        assert!(
                            trial(&fixture, &probe, &fixture.font)
                                .iter()
                                .all(|glyph| glyph.glyph_id != 0)
                        );
                    } else {
                        assert!(!answered);
                        answered = true;
                    } // A successful same-font middle trial still cannot create a font boundary that does not exist.
                    let range = if shortened {
                        0..463
                    } else {
                        request.source_range()
                    };
                    worker
                        .provide_style(
                            request
                                .respond(range, fixture.font.clone(), style())
                                .unwrap(),
                        )
                        .unwrap();
                }
                MarkdownLineItemAnswer::ItemReady(_) => {
                    drop(worker.take_item().unwrap());
                }
                MarkdownLineItemAnswer::NeedsFrontier(_) => {
                    rejected = true;
                    break;
                }
                _ => panic!("未认证的中部或短样式不能发布假EOF"),
            }
        }
        assert!(answered && rejected);
    }
}

#[cfg(target_os = "macos")]
#[path = "middle_tests.rs"]
mod middle_tests;
