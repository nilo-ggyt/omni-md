//! 真实Coordinator→Store→Measure→LineEnd前沿候选；完整oracle只在有限测试中单独建立。
use super::*;

#[cfg(target_os = "macos")]
pub(super) fn mono() -> (
    MarkdownLayoutFont,
    crate::MarkdownLayoutFontBytesResourceObserver,
) {
    let bytes = crate::MarkdownLayoutFontBytes::capture_file(
        std::path::Path::new("/System/Library/Fonts/SFNSMono.ttf"),
        32 * 1024 * 1024,
    )
    .unwrap();
    let observer = bytes.resource_observer();
    let font = MarkdownLayoutFont::resolve(bytes.blob(), ".SFNSMono-Regular", &[]).unwrap();
    drop(bytes);
    (font, observer) // 系统路径仅是当前机器的实际正控，生产仍按完整真实plan认证。
}
pub(super) fn worker(
    fixture: &Fixture,
    font: &MarkdownLayoutFont,
    width: i64,
) -> (MarkdownLineComposer, MarkdownLineRegisteredFont) {
    let catalog = MarkdownLineFontCatalog::new();
    let actual = catalog.register(font).unwrap();
    (
        MarkdownLineComposer::new_with_frontier(
            fixture.source.clone(),
            &fixture.source.authority(),
            catalog,
            width,
            MarkdownLineComposeCancellation::new(),
        )
        .unwrap(),
        actual,
    )
}
pub(super) fn first(
    worker: &mut MarkdownLineComposer,
    font: &MarkdownLineRegisteredFont,
) -> MarkdownComposedLineFacts {
    for _ in 0..100_000 {
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
            MarkdownLineComposeAnswer::LineReady(facts) => return facts,
            MarkdownLineComposeAnswer::NeedsFrontier(reason) => {
                panic!("真实自然space正控不应缺前沿: {reason:?}")
            }
            MarkdownLineComposeAnswer::NeedsResolution(reason) => {
                panic!("实际前缀行尚需后继解析: {reason:?}")
            }
            _ => panic!("首个完整LineEnd前不能假Complete、控制或另行解析"),
        }
    }
    panic!("有限自然机会夹具没有实际LineEnd");
}

#[cfg(target_os = "macos")]
#[test]
fn actual_worker_first_line_shapes_constant_prefix_for_4k_8k_32k_source_and_releases_real_resources()
 {
    let (font, font_resources) = mono();
    let mut work = Vec::new(); // 三档固定元数据，不是产品的历史item或正文缓存。
    for units in [4096usize, 8192, 32768] {
        let mut text = "hello world ".repeat(units.div_ceil(12));
        text.truncate(units); // 有限ASCII夹具精确4K/8K/32K字节，非产品读取或切段规则。
        let fixture = Fixture::new(&text);
        let source_observer = fixture.source.resource_observer();
        let backing = fixture.document.original_backing_observer().unwrap();
        let (mut worker, actual) = worker(&fixture, &font, 80 * 64);
        let first = first(&mut worker, &actual);
        let measured = worker.frontier_work().unwrap();
        assert_eq!(measured.shape_calls, 1);
        assert_eq!(measured.parts, 1);
        assert!(first.source_utf16.end < measured.input_bytes && measured.input_bytes <= 256);
        assert!(measured.source_bytes <= 257);
        work.push((measured, first.source_utf16.end));
        let mut output = worker.into_output().unwrap();
        let output_observer = output.resource_observer();
        let line = output.last_line().unwrap().unwrap();
        let part = output.part(&line, 0).unwrap().unwrap();
        // 同一实际Font的完整有限oracle在work已记录后单独建立；不把它算成产品前沿输入。
        let mut oracle = crate::MarkdownShapingJob::new(
            fixture.source.clone(),
            &fixture.source.authority(),
            font.clone(),
            &font.authority(),
            0..fixture.source.utf16_len(),
            crate::MarkdownShapingOptions {
                direction: crate::MarkdownShapingDirection::LeftToRight,
                script: *b"Latn",
                language: None,
                points: 16.0,
                beginning_of_text: true,
                end_of_text: true,
                features: Vec::new(),
            },
        )
        .unwrap();
        while oracle.status() == crate::MarkdownShapingStatus::Ingesting {
            oracle
                .step(
                    &fixture.source.authority(),
                    &font.authority(),
                    crate::MarkdownShapingBudget::default(),
                )
                .unwrap();
        }
        oracle
            .shape(&fixture.source.authority(), &font.authority())
            .unwrap();
        let mut count = 0;
        for ordinal in 0..oracle.profile().unwrap().glyph_count {
            let glyph = oracle
                .glyph(&fixture.source.authority(), &font.authority(), ordinal)
                .unwrap()
                .unwrap();
            if first.source_utf16.contains(&glyph.source_utf16_cluster) {
                let mut actual_glyph = [glyph];
                output
                    .read_batch(&part, count..count + 1, &mut actual_glyph)
                    .unwrap();
                assert_eq!(actual_glyph[0], glyph);
                count += 1;
            }
        }
        assert_eq!(count, first.glyphs);
        drop(oracle);
        output.close().unwrap();
        let closed = output_observer.snapshot();
        assert!(
            !closed.file_open
                && closed.mapped_bytes == 0
                && !closed.cleanup_failed
                && !closed.reader_failed
        );
        drop(actual);
        drop(fixture);
        assert!(source_observer.payload_released());
        assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
    }
    assert!(work.windows(2).all(|pair| pair[0] == pair[1]));
    eprintln!(
        "composer_frontier_actual_first_line work={work:?} metadata_full_source=true whole_product_pass=false"
    );
    drop(font);
    assert!(
        font_resources.unmap_succeeded()
            && font_resources.mapped_bytes() == 0
            && !font_resources.failed()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn actual_frontier_continuation_keeps_one_soft_line_across_multiple_certified_spans() {
    let (font, _) = mono();
    let fixture = Fixture::new(&"hello world ".repeat(512));
    let (mut worker, actual) = worker(&fixture, &font, 3000 * 64);
    let line = first(&mut worker, &actual);
    let work = worker.frontier_work().unwrap();
    assert!(work.parts >= 2 && work.shape_calls == work.parts);
    assert!(line.parts >= 2 && line.source_utf16.end > 256);
    assert!(line.source_utf16.end < fixture.source.utf16_len());
    let mut output = worker.into_output().unwrap();
    let actual_line = output.last_line().unwrap().unwrap();
    assert_eq!(actual_line.facts(), &line);
    assert!(output.previous_line(&actual_line).unwrap().is_none());
    let mut end = 0;
    for ordinal in 0..line.parts {
        let part = output.part(&actual_line, ordinal).unwrap().unwrap();
        assert_eq!(part.facts().source_utf16.start, end);
        end = part.facts().source_utf16.end;
        output.read_batch(&part, 0..0, &mut []).unwrap();
    }
    assert_eq!(end, line.source_utf16.end);
    output.close().unwrap(); // 前沿片段的结束不是软行结束，真实Measure选中之后才有唯一LineEnd。
}

#[test]
fn actual_frontier_worker_rejects_unproven_plan_without_whole_item_shape_fallback() {
    let fixture = Fixture::new(&"hello world ".repeat(512));
    let (mut worker, actual) = worker(&fixture, &font(), 80 * 64);
    let mut reached = false;
    for _ in 0..100_000 {
        let step = worker.step(MarkdownLineComposeBudget::default()).unwrap();
        assert!(step.metadata_units <= MarkdownLineComposeBudget::default().metadata_units);
        match step.answer {
            MarkdownLineComposeAnswer::Yielded => {}
            MarkdownLineComposeAnswer::NeedStyle(request) => {
                let range = request.source_range();
                worker
                    .provide_style(request.respond_registered(range, &actual, style()).unwrap())
                    .unwrap();
            }
            MarkdownLineComposeAnswer::NeedsFrontier(reason) => {
                assert_eq!(reason, MarkdownLineFrontierReason::UnsupportedPlan);
                let work = worker.frontier_work().unwrap();
                assert_eq!(
                    (
                        work.source_bytes,
                        work.input_bytes,
                        work.context_bytes,
                        work.shape_calls,
                        work.parts,
                        work.metadata_glyphs
                    ),
                    (0, 0, 0, 0, 0, 0)
                );
                // Body闭包确实读取原计划后才能拒绝Roboto；该成本必须报告，不能伪记为零。
                assert!(work.plan_records > 0);
                reached = true;
                break;
            }
            _ => panic!("未证Roboto真实计划不能整item回退或发布假行"),
        }
    }
    assert!(reached && worker.into_output().is_err());
}

#[cfg(target_os = "macos")]
#[test]
fn actual_frontier_unread_non_ascii_suffix_cannot_borrow_ascii_word_proof() {
    let (font, _) = mono();
    let fixture = Fixture::new(&format!("{}\u{ad}{}", "a".repeat(512), "a".repeat(512)));
    let (mut worker, actual) = worker(&fixture, &font, 80 * 64);
    for _ in 0..100_000 {
        match worker
            .step(MarkdownLineComposeBudget::default())
            .unwrap()
            .answer
        {
            MarkdownLineComposeAnswer::Yielded => {}
            MarkdownLineComposeAnswer::NeedStyle(request) => {
                let range = request.source_range();
                worker
                    .provide_style(request.respond_registered(range, &actual, style()).unwrap())
                    .unwrap();
            }
            MarkdownLineComposeAnswer::NeedsFrontier(reason) => {
                assert_eq!(reason, MarkdownLineFrontierReason::UncertifiedWord);
                let work = worker.frontier_work().unwrap();
                assert_eq!(
                    (
                        work.source_bytes,
                        work.input_bytes,
                        work.shape_calls,
                        work.parts
                    ),
                    (257, 0, 0, 0)
                );
                assert!(worker.into_output().is_err());
                return;
            }
            _ => panic!("未读词尾有非ASCII时不能把有限PartialJob算成真实LineReady"),
        }
    }
    panic!("未认证字符域不能耗尽整源后假完成");
}

#[cfg(target_os = "macos")]
#[test]
fn authenticated_word_prefix_reaches_first_line_with_constant_glyph_work_and_exact_fields() {
    let (font, _) = mono();
    let mut comparisons = Vec::new(); // 仅三个有限测试工作量标量，不进入生产item历史。
    for units in [4096usize, 8192, 32768] {
        let fixture = Fixture::new(&"a".repeat(units));
        let (mut worker, actual) = worker(&fixture, &font, 80 * 64);
        let mut bytes_after_style = 0usize;
        let mut shapes = 0usize;
        let mut supplied_style = false;
        let mut first_line = None;
        for _ in 0..100_000 {
            let step = worker.step(MarkdownLineComposeBudget::default()).unwrap();
            if supplied_style {
                bytes_after_style += step.bytes;
                shapes += step.shapes;
            }
            match step.answer {
                MarkdownLineComposeAnswer::Yielded => {}
                MarkdownLineComposeAnswer::NeedStyle(request) => {
                    let range = request.source_range();
                    worker
                        .provide_style(request.respond_registered(range, &actual, style()).unwrap())
                        .unwrap();
                    supplied_style = true;
                }
                MarkdownLineComposeAnswer::LineReady(facts) => {
                    first_line = Some(facts);
                    break;
                }
                _ => panic!("原词/非负/真实GCB全部认证后必须产出实际紧急LineEnd"),
            }
        }
        let facts = first_line.expect("有限已认证词前缀没有签出真实LineEnd");
        let work = worker.frontier_work().unwrap();
        eprintln!(
            "word_first_line units={units} frontier={work:?} bytes_after_style={bytes_after_style} shapes={shapes} line={facts:?}"
        );
        assert_eq!(
            (work.parts, work.shape_calls, work.input_bytes),
            (1, 1, 256)
        );
        assert!(work.source_bytes <= 257 && bytes_after_style < 1024);
        assert_eq!(shapes, work.shape_calls); // 安全原glyph直接签行，不能再隐藏overflow候选/已fit前缀重形。
        assert!(facts.source_utf16.end > 0 && facts.source_utf16.end < 256);
        comparisons.push((work, bytes_after_style, shapes, facts.source_utf16.end));
        let mut output = worker.publish_ready_prefix().unwrap().unwrap();
        let line = output.last_line().unwrap().unwrap();
        assert_eq!(line.facts(), &facts);
        let part = output.part(&line, 0).unwrap().unwrap();
        // 全item只在独立有限oracle建立；产品work已先记录，不能把oracle输入藏进候选计数。
        let mut whole = crate::MarkdownShapingJob::new(
            fixture.source.clone(),
            &fixture.source.authority(),
            font.clone(),
            &font.authority(),
            0..units,
            crate::MarkdownShapingOptions {
                direction: crate::MarkdownShapingDirection::LeftToRight,
                script: *b"Latn",
                language: None,
                points: 16.0,
                beginning_of_text: true,
                end_of_text: true,
                features: Vec::new(),
            },
        )
        .unwrap();
        while whole.status() == crate::MarkdownShapingStatus::Ingesting {
            whole
                .step(
                    &fixture.source.authority(),
                    &font.authority(),
                    crate::MarkdownShapingBudget::default(),
                )
                .unwrap();
        }
        whole
            .shape(&fixture.source.authority(), &font.authority())
            .unwrap();
        let mut copied = 0;
        for at in 0..whole.profile().unwrap().glyph_count {
            let expected = whole
                .glyph(&fixture.source.authority(), &font.authority(), at)
                .unwrap()
                .unwrap();
            if facts.source_utf16.contains(&expected.source_utf16_cluster) {
                let mut glyph = [expected];
                output
                    .read_batch(&part, copied..copied + 1, &mut glyph)
                    .unwrap();
                assert_eq!(glyph[0], expected);
                copied += 1;
            }
        }
        assert_eq!(copied, facts.glyphs);
        output.close().unwrap();
    }
    assert!(comparisons.windows(2).all(|pair| pair[0] == pair[1]));
    eprintln!(
        "composer_word_prefix_first_line work={comparisons:?} metadata_full_source=true whole_product_pass=false"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn word_prefix_output_snapshots_resume_same_worker_across_real_frontier_continuations() {
    let (font, _) = mono();
    let fixture = Fixture::new(&"a".repeat(4099));
    let (mut worker, actual) = worker(&fixture, &font, 80 * 64);
    let mut end = 0;
    let mut first_snapshot = None;
    for ordinal in 0..40 {
        let facts = first(&mut worker, &actual);
        assert_eq!((facts.ordinal, facts.source_utf16.start), (ordinal, end));
        end = facts.source_utf16.end;
        if ordinal == 11 {
            first_snapshot = worker.publish_ready_prefix().unwrap();
        }
    }
    let mut first_snapshot = first_snapshot.unwrap();
    let old_end = first_snapshot.source_range().end;
    let mut current = worker.publish_ready_prefix().unwrap().unwrap();
    assert_eq!(current.source_range().end, end);
    assert!(old_end < 256 && end > 256); // 同owner真child归还后续片，不能每视口从0复造。
    let work = worker.frontier_work().unwrap();
    assert!(work.parts >= 2 && work.input_bytes <= 512);
    assert_eq!(
        first_snapshot.last_line().unwrap().unwrap().facts().ordinal,
        11
    );
    assert_eq!(current.last_line().unwrap().unwrap().facts().ordinal, 39);
    assert_eq!(first_snapshot.source_range().end, old_end);
    first_snapshot.close().unwrap();
    current.close().unwrap();
    let mut count = 40;
    let mut complete = false;
    for _ in 0..100_000 {
        match worker
            .step(MarkdownLineComposeBudget::default())
            .unwrap_or_else(|error| {
                panic!("完整词续排在已提交行{count}/源端点{end}失败: {error:?}")
            })
            .answer
        {
            MarkdownLineComposeAnswer::Yielded => {}
            MarkdownLineComposeAnswer::LineReady(facts) => {
                assert_eq!((facts.ordinal, facts.source_utf16.start), (count, end));
                end = facts.source_utf16.end;
                count += 1;
            }
            MarkdownLineComposeAnswer::Complete => {
                complete = true;
                break;
            }
            _ => panic!("最终不足256的原词短片仍须保留原许可，不可缺行或伪造EOF"),
        }
    }
    assert!(complete && end == fixture.source.utf16_len());
    assert_eq!(end, 4099);
    let mut final_output = worker.publish_ready_prefix().unwrap().unwrap();
    let final_line = final_output.last_line().unwrap().unwrap();
    assert_eq!(final_line.facts().source_utf16.end, 4099);
    assert_eq!(final_line.facts().ordinal, count - 1);
    final_output.close().unwrap();
}

#[cfg(target_os = "macos")]
#[test]
fn private_word_fact_survives_actual_job_return_without_extending_stored_glyph_coverage() {
    use super::super::store::GlyphStore;
    use crate::markdown_soft_line_measure::provider::GlyphProvider;
    let (font, _) = mono();
    let fixture = Fixture::new(&"a".repeat(4096));
    let catalog = MarkdownLineFontCatalog::new();
    let registered = catalog.register(&font).unwrap();
    let cancel = MarkdownLineComposeCancellation::new();
    let authority = fixture.source.authority();
    let mut coordinator = MarkdownLineItemCoordinator::new_with_frontier(
        fixture.source.clone(),
        &authority,
        cancel.clone(),
    )
    .unwrap();
    let mut item = None;
    for _ in 0..100_000 {
        match coordinator
            .step(MarkdownLineComposeBudget::default())
            .unwrap()
            .answer
        {
            MarkdownLineItemAnswer::Yielded => {}
            MarkdownLineItemAnswer::NeedStyle(request) => {
                let range = request.source_range();
                coordinator
                    .provide_style(
                        request
                            .respond_registered(range, &registered, style())
                            .unwrap(),
                    )
                    .unwrap();
            }
            MarkdownLineItemAnswer::ItemReady(_) => {
                item = Some(coordinator.take_item().unwrap());
                break;
            }
            _ => panic!("已认证单词producer没有交出真实有限ReadyJob"),
        }
    }
    let mut item = item.expect("有限前沿producer未完成");
    let job_resources = item.job.resource_observer();
    let mut store =
        GlyphStore::new(fixture.source.clone(), &authority, catalog, cancel.clone()).unwrap();
    store.begin(&mut item).unwrap();
    let mut proof = None;
    for _ in 0..10_000 {
        if let Some(actual) = store.step(&mut item, 256).unwrap() {
            proof = Some(actual);
            break;
        }
    }
    let proof = proof.expect("有限item没有完整签入唯一Store");
    drop(item);
    assert_eq!(job_resources.snapshot().files_open, 0);
    assert_eq!(job_resources.snapshot().mapped_bytes, 0);
    let mut provider = store.borrow(&proof).unwrap();
    assert_eq!(
        GlyphProvider::require(&mut provider, &authority, &font.authority())
            .unwrap()
            .source_utf16,
        0..256
    );
    assert_eq!(
        GlyphProvider::frontier_word_end(&mut provider, &authority, &font.authority()).unwrap(),
        Some(4096)
    );
    let facts = GlyphProvider::require(&mut provider, &authority, &font.authority()).unwrap();
    assert_eq!(facts.source_utf16, 0..256); // require已核真实glyphEOF；词尾不能升级为已shape源范围。
    cancel.cancel();
    assert!(matches!(
        GlyphProvider::frontier_word_end(&mut provider, &authority, &font.authority()),
        Err(crate::MarkdownSoftLineMeasureError::Cancelled)
    ));
}

#[cfg(target_os = "macos")]
#[test]
fn word_prefix_preserves_complete_first_grapheme_when_its_actual_advance_overflows() {
    let (font, _) = mono();
    let fixture = Fixture::new(&"a".repeat(4096));
    let (mut worker, actual) = worker(&fixture, &font, 1);
    let line = first(&mut worker, &actual);
    assert_eq!(line.source_utf16, 0..1);
    assert_eq!(line.kind, MarkdownComposedLineKind::Overflow);
    assert!(line.x_advance_26_6 > 1);
    let mut output = worker.publish_ready_prefix().unwrap().unwrap();
    let actual = output.last_line().unwrap().unwrap();
    assert_eq!(actual.facts(), &line);
    assert_eq!(output.grapheme_ordinal(0).unwrap(), Some(0));
    assert_eq!(output.grapheme_ordinal(1).unwrap(), Some(1));
    output.close().unwrap();
}
