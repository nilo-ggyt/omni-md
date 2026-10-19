//! 原GCB坐标与真实完整整形双重差分；字体文件控制不替代Native排版/首屏验收。
use super::*;

fn ready_breaks(fixture: &mut Fixture) {
    // 此前沿消费协调器已经完成的同一张GCB表；准备工作单独执行，不能冒充首part免费扫描。
    for _ in 0..1_000_000 {
        if fixture
            .breaks
            .step(
                &fixture.authority,
                MarkdownLineBreakBudget {
                    bytes: 4096,
                    scalars: 4096,
                    records: 4096,
                },
            )
            .unwrap()
            .ready
        {
            return;
        }
    }
    panic!("有限控制的原断点表未完成")
}

fn options(fixture: &Fixture, nonzero: bool) -> MarkdownShapingOptions {
    let mut options = fixture.options();
    options.beginning_of_text = !nonzero;
    options
}

fn begin(
    fixture: &Fixture,
    range: Range<usize>,
    options: MarkdownShapingOptions,
) -> MarkdownShapingFrontierJob {
    MarkdownShapingFrontierJob::new(
        fixture.source.clone(),
        &fixture.authority,
        fixture.font.clone(),
        &fixture.font_authority,
        range,
        options,
        MarkdownShapingCancellation::default(),
    )
    .unwrap()
}

fn full(
    fixture: &Fixture,
    range: Range<usize>,
    options: MarkdownShapingOptions,
) -> MarkdownShapingJob {
    // 独立完整任务不使用局部plan、center view或Frontier给出的任何glyph结果。
    let mut job = MarkdownShapingJob::new(
        fixture.source.clone(),
        &fixture.authority,
        fixture.font.clone(),
        &fixture.font_authority,
        range,
        options,
    )
    .unwrap();
    while job.status() == MarkdownShapingStatus::Ingesting {
        job.step(
            &fixture.authority,
            &fixture.font_authority,
            MarkdownShapingBudget::default(),
        )
        .unwrap();
    }
    job.shape(&fixture.authority, &fixture.font_authority)
        .unwrap();
    job
}

#[derive(Default, Debug, PartialEq, Eq)]
struct Work {
    source: usize,
    input: usize,
    scalars: usize,
    context: usize,
    plans: usize,
    glyphs: usize,
    boundaries: usize,
    shapes: usize,
}

fn first(
    fixture: &mut Fixture,
    frontier: &mut MarkdownShapingFrontierJob,
    bytes: usize,
    metadata: usize,
) -> (MarkdownShapingFrontierPart, Work) {
    let mut work = Work::default();
    for _ in 0..100_000 {
        let step = frontier
            .step(
                &fixture.authority,
                &fixture.font_authority,
                &mut fixture.breaks,
                MarkdownShapingBudget {
                    bytes,
                    scalars: bytes,
                },
                metadata,
            )
            .unwrap();
        assert!(step.source_bytes + step.input_bytes <= bytes, "{step:?}");
        assert!(step.scalars <= bytes && step.shapes <= 1, "{step:?}");
        assert!(
            step.plan_records + step.metadata_glyphs + step.boundary_records <= metadata,
            "{step:?}"
        );
        work.source += step.source_bytes;
        work.input += step.input_bytes;
        work.scalars += step.scalars;
        work.context += step.context_bytes;
        work.plans += step.plan_records;
        work.glyphs += step.metadata_glyphs;
        work.boundaries += step.boundary_records;
        work.shapes += step.shapes;
        match step.answer {
            MarkdownShapingFrontierAnswer::Pending => {}
            MarkdownShapingFrontierAnswer::PartReady => {
                return (
                    frontier
                        .take_part(&fixture.authority, &fixture.font_authority)
                        .unwrap(),
                    work,
                );
            }
            answer => panic!("真实Unicode前沿没有认证: {answer:?}; work={work:?}"),
        }
    }
    panic!("有限fixture未结束: {work:?}")
}

fn compare(fixture: &Fixture, child: &mut MarkdownShapingJob, full: &mut MarkdownShapingJob) {
    let range = child.source_range();
    let mut expected = 0;
    for ordinal in 0..full.profile().unwrap().glyph_count {
        let glyph = full
            .glyph(&fixture.authority, &fixture.font_authority, ordinal)
            .unwrap()
            .unwrap();
        if range.contains(&glyph.source_utf16_cluster) {
            assert_eq!(
                child
                    .glyph(&fixture.authority, &fixture.font_authority, expected)
                    .unwrap(),
                Some(glyph),
                "center={range:?} ordinal={expected}"
            );
            assert_ne!(glyph.glyph_id, 0);
            expected += 1;
        }
    }
    assert_eq!(expected, child.profile().unwrap().glyph_count);
    assert!(
        child
            .glyph(&fixture.authority, &fixture.font_authority, expected)
            .unwrap()
            .is_none()
    );
}

#[test]
fn one_byte_one_record_preserves_utf8_gcb_fifo_and_exact_glyphs() {
    let text = "éàüö".repeat(67);
    let mut fixture = Fixture::new(&text, independent_font());
    ready_breaks(&mut fixture);
    let range = 0..fixture.source.utf16_len();
    let mut frontier = begin(&fixture, range.clone(), options(&fixture, false));
    let mut oracle = full(&fixture, range.clone(), options(&fixture, false));
    let mut covered = 0;
    let mut parts = 0;
    while covered < range.end {
        let (part, work) = first(&mut fixture, &mut frontier, 1, 1);
        let (mut child, continuation) = part.into_parts();
        assert_eq!(child.source_range().start, covered);
        assert!(child.source_range().len() <= 256 && work.shapes == 1);
        compare(&fixture, &mut child, &mut oracle);
        covered = child.source_range().end;
        let resources = child.resource_observer();
        drop(child);
        assert_eq!(resources.snapshot().files_open, 0);
        assert_eq!(resources.snapshot().mappings_open, 0);
        frontier
            .resume(&fixture.authority, &fixture.font_authority, continuation)
            .unwrap();
        parts += 1;
    }
    assert_eq!(parts, 2);
    assert_eq!(
        frontier
            .step(
                &fixture.authority,
                &fixture.font_authority,
                &mut fixture.breaks,
                MarkdownShapingBudget {
                    bytes: 1,
                    scalars: 1
                },
                1
            )
            .unwrap()
            .answer,
        MarkdownShapingFrontierAnswer::Complete
    );
}

#[test]
fn actual_multi_scalar_grapheme_is_not_cut_and_decoded_failure_is_counted() {
    // 这个真实GCB恰好4字节，不能只以byte长度或首标量类别认成独立scalar。
    let mut fixture = Fixture::new("é\u{301}x", independent_font());
    ready_breaks(&mut fixture);
    let mut frontier = begin(
        &fixture,
        0..fixture.source.utf16_len(),
        options(&fixture, false),
    );
    let mut scalars = 0;
    for _ in 0..1024 {
        let step = frontier
            .step(
                &fixture.authority,
                &fixture.font_authority,
                &mut fixture.breaks,
                MarkdownShapingBudget {
                    bytes: 1,
                    scalars: 1,
                },
                1,
            )
            .unwrap();
        scalars += step.scalars;
        assert!(step.source_bytes + step.input_bytes <= 1);
        assert!(step.plan_records + step.metadata_glyphs + step.boundary_records <= 1);
        if step.answer != MarkdownShapingFrontierAnswer::Pending {
            assert_eq!(
                step.answer,
                MarkdownShapingFrontierAnswer::NeedsFrontier(
                    MarkdownShapingFrontierReason::NonSimpleGrapheme
                )
            );
            assert_eq!(scalars, 1);
            assert!(frontier.child.is_none());
            return;
        }
    }
    panic!("不允许把未解析GCB留成假PartReady")
}

#[test]
fn original_source_font_cancel_and_actual_io_revoke_unicode_route() {
    let mut fixture = Fixture::new(&"é".repeat(600), independent_font());
    ready_breaks(&mut fixture);
    let source_observer = fixture.source.resource_observer();
    let backing = fixture.document.original_backing_observer().unwrap();
    let other = MarkdownLayoutTextSource::capture(&fixture.document);
    let another_font = independent_font();
    let mut wrong_source = begin(&fixture, 0..600, options(&fixture, false));
    assert!(matches!(
        wrong_source.step(
            &other.authority(),
            &fixture.font_authority,
            &mut fixture.breaks,
            MarkdownShapingBudget::default(),
            128
        ),
        Err(MarkdownShapingFrontierError::Shaping(
            MarkdownShapingError::SourceMismatch
        ))
    ));
    let mut wrong_font = begin(&fixture, 0..600, options(&fixture, false));
    assert!(matches!(
        wrong_font.step(
            &fixture.authority,
            &another_font.authority(),
            &mut fixture.breaks,
            MarkdownShapingBudget::default(),
            128
        ),
        Err(MarkdownShapingFrontierError::Shaping(
            MarkdownShapingError::FontMismatch
        ))
    ));
    let mut cancelled = begin(&fixture, 0..600, options(&fixture, false));
    cancelled.cancellation.request();
    assert!(matches!(
        cancelled.step(
            &fixture.authority,
            &fixture.font_authority,
            &mut fixture.breaks,
            MarkdownShapingBudget::default(),
            128
        ),
        Err(MarkdownShapingFrontierError::Shaping(
            MarkdownShapingError::Cancelled
        ))
    ));
    let mut io = begin(&fixture, 0..600, options(&fixture, false));
    fixture.source.visit_byte_range(0..1, |_| Ok(())).unwrap();
    fixture.document.fail_next_original_reclaim_for_test();
    assert!(matches!(
        fixture.document.reclaim_original_read_windows_for_test(),
        Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
            libc::EIO
        ))
    ));
    io.cancellation.request();
    assert!(matches!(
        io.step(
            &fixture.authority,
            &fixture.font_authority,
            &mut fixture.breaks,
            MarkdownShapingBudget::default(),
            128
        ),
        Err(MarkdownShapingFrontierError::Shaping(
            MarkdownShapingError::Source(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                libc::EIO
            ))
        ))
    ));
    drop((wrong_source, wrong_font, cancelled, io, other, fixture));
    assert!(source_observer.payload_released() && backing.unmap_succeeded());
}

fn ping_fang() -> (
    MarkdownLayoutFont,
    crate::MarkdownLayoutFontBytesResourceObserver,
) {
    // 这个路径/face/轴值由真实Body14字体回退控制得到；生产证书不按路径或名字放行。
    let path = std::path::PathBuf::from(
        std::env::var_os("OMNI_FRONTIER_CJK_FONT").expect("需要真实CJK字体源"),
    );
    let bytes = crate::MarkdownLayoutFontBytes::capture_file(&path, 128 * 1024 * 1024).unwrap();
    let observer = bytes.resource_observer();
    let name = read_fonts::FontRef::from_index(bytes.as_ref(), 0)
        .unwrap()
        .table_data(read_fonts::types::Tag::new(b"name"))
        .unwrap()
        .as_bytes()
        .to_vec();
    let axes = [
        crate::MarkdownLayoutFontAxis {
            tag: *b"HGHT",
            value: 500.0,
        },
        crate::MarkdownLayoutFontAxis {
            tag: *b"WDTH",
            value: 500.0,
        },
        crate::MarkdownLayoutFontAxis {
            tag: *b"wght",
            value: 400.0,
        },
    ];
    let font =
        MarkdownLayoutFont::resolve_name_table_at_size(bytes.blob(), &name, &axes, 14.0).unwrap();
    assert_eq!(font.metadata().face_index, 0);
    assert_eq!(font.metadata().face_count, 32);
    assert_eq!(font.metadata().glyph_count, 47102);
    assert_eq!(font.metadata().units_per_em, 1028);
    font.verify_design_coordinates(&[(*b"HGHT", 500.0), (*b"WDTH", 500.0), (*b"wght", 400.0)])
        .unwrap();
    (font, observer)
}

fn hani_options() -> MarkdownShapingOptions {
    MarkdownShapingOptions {
        direction: MarkdownShapingDirection::LeftToRight,
        script: *b"Hani",
        language: None,
        points: 14.0,
        beginning_of_text: false,
        end_of_text: true,
        features: Vec::new(),
    }
}

#[test]
#[ignore = "需要真实Native字体回退抓取的PingFang源；设置OMNI_FRONTIER_CJK_FONT"]
fn actual_cjk_14pt_first_part_shape_work_does_not_grow_with_item() {
    let (font, mapping) = ping_fang();
    let font_drop = font.resource_observer();
    let mut previous = None;
    for length in [4096, 8192, 32768] {
        let text = format!(
            "前置 {}",
            "中文段落，标点。汉字测试".repeat(length / 12 + 1)
        );
        let mut fixture = Fixture::new(&text, font.clone());
        ready_breaks(&mut fixture);
        let range = 3..fixture.source.utf16_len();
        let mut frontier = begin(&fixture, range.clone(), hani_options());
        let (part, work) = first(&mut fixture, &mut frontier, 7, 1);
        let (mut child, continuation) = part.into_parts();
        assert_eq!(child.source_range(), 3..259);
        assert_eq!(work.shapes, 1);
        assert!(
            work.input <= 257 * 3 && work.source <= 257 * 3 + 1,
            "{work:?}"
        );
        let mut oracle = full(&fixture, range, hani_options());
        compare(&fixture, &mut child, &mut oracle);
        // 二分定位原GCB索引的记录数可随logN变化；正文/整形输入和字体元数据必须恒定。
        let constant = (
            work.source,
            work.input,
            work.scalars,
            work.context,
            work.plans,
            work.glyphs,
            work.shapes,
        );
        assert!(previous.as_ref().is_none_or(|old| *old == constant));
        previous = Some(constant);
        eprintln!(
            "cjk_actual_first length={length} range={:?} work={work:?}",
            child.source_range()
        );
        let child_drop = child.resource_observer();
        drop(child);
        assert_eq!(child_drop.snapshot().files_open, 0);
        assert_eq!(child_drop.snapshot().mappings_open, 0);
        frontier
            .resume(&fixture.authority, &fixture.font_authority, continuation)
            .unwrap();
    }
    drop(font);
    assert!(font_drop.payload_released());
    assert!(mapping.mapping_drop_completed() && mapping.unmap_succeeded() && !mapping.failed());
    assert_eq!(mapping.mapped_bytes(), 0);
    assert!(mapping.source_descriptor_closed() && mapping.backing_descriptor_closed());
}

#[test]
#[ignore = "需要真实Native字体回退抓取的PingFang源；设置OMNI_FRONTIER_CJK_FONT"]
fn actual_cjk_14pt_contiguous_centers_match_every_full_oracle_field_and_release() {
    let (font, mapping) = ping_fang();
    let text = format!("前置 {}尾", "中文，标点。汉字测试".repeat(410));
    let mut fixture = Fixture::new(&text, font);
    ready_breaks(&mut fixture);
    let source = fixture.source.resource_observer();
    let backing = fixture.document.original_backing_observer().unwrap();
    let range = 3..fixture.source.utf16_len();
    let mut frontier = begin(&fixture, range.clone(), hani_options());
    let mut oracle = full(&fixture, range.clone(), hani_options());
    let mut covered = range.start;
    let mut parts = 0;
    while covered < range.end {
        let (part, _) = first(&mut fixture, &mut frontier, 31, 1);
        let (mut child, ticket) = part.into_parts();
        assert_eq!(child.source_range().start, covered);
        compare(&fixture, &mut child, &mut oracle);
        covered = child.source_range().end;
        let observer = child.resource_observer();
        drop(child);
        assert_eq!(observer.snapshot().files_open, 0);
        assert_eq!(observer.snapshot().mappings_open, 0);
        frontier
            .resume(&fixture.authority, &fixture.font_authority, ticket)
            .unwrap();
        parts += 1;
    }
    assert_eq!(covered, range.end);
    eprintln!("cjk_actual_contiguous parts={parts} end={covered} every_glyph_exact=true");
    drop((oracle, frontier, fixture));
    assert!(source.payload_released() && backing.unmap_succeeded());
    assert!(mapping.mapping_drop_completed() && mapping.unmap_succeeded() && !mapping.failed());
    assert_eq!(mapping.mapped_bytes(), 0);
}

#[test]
#[ignore = "需要真实Native字体回退抓取的PingFang源；设置OMNI_FRONTIER_CJK_FONT"]
fn actual_cjk_non_bmp_14pt_first_part_keeps_glyph_ordinals_separate_from_utf16() {
    let (font, mapping) = ping_fang();
    let mut previous = None;
    for graphemes in [4096, 8192, 32768] {
        // U+20BB7由同一真实face的cmap提供glyph37154；这里只把它当固定测试输入，
        // 生产仍逐原GCB/normalizer/实际最终glyph认证，不能按Han码段或字体名放行。
        let text = format!("前置 {}", "𠮷".repeat(graphemes));
        let mut fixture = Fixture::new(&text, font.clone());
        ready_breaks(&mut fixture);
        assert!(matches!(
            MarkdownShapingFrontierJob::new(
                fixture.source.clone(),
                &fixture.authority,
                fixture.font.clone(),
                &fixture.font_authority,
                3..4,
                hani_options(),
                MarkdownShapingCancellation::default(),
            ),
            Err(MarkdownShapingFrontierError::Shaping(
                MarkdownShapingError::Source(MarkdownDocumentBufferError::NotUnicodeBoundary)
            ))
        )); // 半个代理项不能由普通Range升级为中心；失败前不分配child。
        let range = 3..fixture.source.utf16_len();
        let mut frontier = begin(&fixture, range.clone(), hani_options());
        let (part, work) = first(&mut fixture, &mut frontier, 7, 1);
        let (mut child, continuation) = part.into_parts();
        assert_eq!(child.source_range(), 3..515);
        assert_eq!(child.profile().unwrap().glyph_count, 256);
        assert_eq!(
            child
                .glyph(&fixture.authority, &fixture.font_authority, 255)
                .unwrap()
                .unwrap()
                .source_utf16_cluster,
            513,
        ); // 第255条glyph的cluster按全局UTF16步进2，ordinal仍只步进1。
        assert_eq!(work.shapes, 1);
        assert!(
            work.input <= 257 * 4 && work.source <= 257 * 4 + 1,
            "{work:?}"
        );
        let mut oracle = full(&fixture, range, hani_options());
        compare(&fixture, &mut child, &mut oracle);
        let constant = (
            work.source,
            work.input,
            work.scalars,
            work.context,
            work.plans,
            work.glyphs,
            work.shapes,
        );
        assert!(previous.as_ref().is_none_or(|old| *old == constant));
        previous = Some(constant);
        eprintln!(
            "cjk_actual_non_bmp_first graphemes={graphemes} range={:?} work={work:?}",
            child.source_range()
        );
        let resources = child.resource_observer();
        drop(child);
        assert_eq!(resources.snapshot().files_open, 0);
        assert_eq!(resources.snapshot().mappings_open, 0);
        frontier
            .resume(&fixture.authority, &fixture.font_authority, continuation)
            .unwrap();
    }
    drop(font);
    assert!(mapping.mapping_drop_completed() && mapping.unmap_succeeded() && !mapping.failed());
    assert_eq!(mapping.mapped_bytes(), 0);
    assert!(mapping.source_descriptor_closed() && mapping.backing_descriptor_closed());
}

#[test]
#[ignore = "需要真实Native字体回退抓取的PingFang源；设置OMNI_FRONTIER_CJK_FONT"]
fn actual_cjk_non_bmp_bmp_punctuation_centers_cover_source_exactly_and_really_release() {
    let (font, mapping) = ping_fang();
    // 起点也含非BMP：byte8与UTF16 4并不相同，后继混合中心的长度更不能当glyph数量。
    let text = format!("前🙂 {}𠮷", "中𠮷，文。𠮷汉".repeat(601));
    let mut fixture = Fixture::new(&text, font);
    ready_breaks(&mut fixture);
    let source = fixture.source.resource_observer();
    let backing = fixture.document.original_backing_observer().unwrap();
    let range = 4..fixture.source.utf16_len();
    let mut frontier = begin(&fixture, range.clone(), hani_options());
    let mut oracle = full(&fixture, range.clone(), hani_options());
    let mut covered = range.start;
    let mut parts = 0;
    let mut glyphs = 0;
    while covered < range.end {
        let (part, _) = first(&mut fixture, &mut frontier, 1, 1);
        let (mut child, ticket) = part.into_parts();
        let center = child.source_range();
        assert_eq!(center.start, covered);
        let count = child.profile().unwrap().glyph_count;
        assert!(count <= 256 && center.len() > count);
        compare(&fixture, &mut child, &mut oracle);
        glyphs += count;
        covered = center.end;
        let observer = child.resource_observer();
        drop(child);
        assert_eq!(observer.snapshot().files_open, 0);
        assert_eq!(observer.snapshot().mappings_open, 0);
        frontier
            .resume(&fixture.authority, &fixture.font_authority, ticket)
            .unwrap();
        parts += 1;
    }
    assert_eq!(covered, range.end);
    assert_eq!(glyphs, oracle.profile().unwrap().glyph_count);
    eprintln!(
        "cjk_actual_non_bmp_contiguous parts={parts} glyphs={glyphs} end={covered} every_glyph_exact=true"
    );
    drop((oracle, frontier, fixture));
    assert!(source.payload_released() && backing.unmap_succeeded());
    assert!(mapping.mapping_drop_completed() && mapping.unmap_succeeded() && !mapping.failed());
    assert_eq!(mapping.mapped_bytes(), 0);
    assert!(mapping.source_descriptor_closed() && mapping.backing_descriptor_closed());
}

fn measure_first(
    fixture: &mut Fixture,
    child: &mut MarkdownShapingJob,
    width: i64,
) -> MarkdownSoftLineMeasureChoice {
    // 这就是原consumer的同一cluster索引；不为前沿额外造索引或从公开Range伪造安全断点。
    let mut clusters =
        MarkdownGlyphClusterIndex::new(child, &fixture.authority, &fixture.font_authority).unwrap();
    while clusters.status() != MarkdownGlyphClusterStatus::Ready {
        clusters.step(child, 1).unwrap();
    }
    let start = child.source_range().start;
    let mut measure = MarkdownSoftLineMeasureJob::new(
        fixture.source.clone(),
        &fixture.authority,
        &fixture.font_authority,
        child,
        &mut fixture.breaks,
        MarkdownSoftLineMeasureRequest {
            start_utf16: start,
            width_26_6: width,
            cancellation: MarkdownSoftLineMeasureCancellation::default(),
        },
    )
    .unwrap();
    for _ in 0..8192 {
        let step = measure
            .step(
                child,
                &mut clusters,
                &mut fixture.breaks,
                MarkdownSoftLineMeasureBudget {
                    clusters: 1,
                    break_queries: 1,
                },
            )
            .unwrap();
        assert!(step.clusters <= 1 && step.break_queries <= 1);
        if let MarkdownSoftLineMeasureAnswer::Selected(choice) = step.answer {
            return choice;
        }
    }
    panic!("有限center的原Measure未返回真实阶段")
}

#[test]
#[ignore = "需要真实Native字体回退抓取的PingFang源；设置OMNI_FRONTIER_CJK_FONT"]
fn actual_cjk_measure_finishes_natural_first_line_but_preserves_unproved_punctuation_chain() {
    let (font, mapping) = ping_fang();
    let mut fixture = Fixture::new(&format!("前置 {}", "中文汉字".repeat(1024)), font.clone());
    ready_breaks(&mut fixture);
    let range = 3..fixture.source.utf16_len();
    let mut frontier = begin(&fixture, range.clone(), hani_options());
    let (part, _) = first(&mut fixture, &mut frontier, 7, 1);
    let (mut child, ticket) = part.into_parts();
    let mut oracle = full(&fixture, range, hani_options());
    compare(&fixture, &mut child, &mut oracle);
    let mut width = 0;
    for ordinal in 0..6 {
        let glyph = oracle
            .glyph(&fixture.authority, &fixture.font_authority, ordinal)
            .unwrap()
            .unwrap();
        assert!(glyph.x_advance > 0);
        width += i64::from(glyph.x_advance);
    } // 独立完整结果给出前6个真实GCB宽度，不把字号或汉字码段当advance。
    let choice = measure_first(&mut fixture, &mut child, width);
    match choice {
        MarkdownSoftLineMeasureChoice::MeasuredBreak(value) => {
            assert_eq!(value.source_utf16, 3..9);
            assert_eq!(value.x_advance_26_6, width);
            assert_eq!(value.boundary.utf16, 9);
        }
        other => panic!("普通Han自然机会未完成首行: {other:?}"),
    }
    drop(child);
    frontier
        .resume(&fixture.authority, &fixture.font_authority, ticket)
        .unwrap();
    drop((oracle, frontier, fixture));

    // OP链的UAX14真NoBreak不能由single-GCB shape证书冒充词前缀非负证明。
    // 当前中心虽早已超宽，原Measure仍须真实后继，不能因测试想要lazy就伪造Emergency。
    let mut fixture = Fixture::new(&format!("前置 {}中", "（".repeat(4096)), font.clone());
    ready_breaks(&mut fixture);
    let mut frontier = begin(&fixture, 3..fixture.source.utf16_len(), hani_options());
    let (part, _) = first(&mut fixture, &mut frontier, 7, 1);
    let (mut child, ticket) = part.into_parts();
    assert!(
        child
            .frontier_word_end(&fixture.authority, &fixture.font_authority)
            .unwrap()
            .is_none()
    );
    let choice = measure_first(&mut fixture, &mut child, 1);
    match choice {
        MarkdownSoftLineMeasureChoice::NeedNextItem {
            source_utf16,
            x_advance_26_6,
            last_fitting,
        } => {
            assert_eq!(source_utf16, 3..259);
            assert!(x_advance_26_6 > 1);
            assert!(last_fitting.is_none());
        }
        other => panic!("未认证的OP链不能偷签首行: {other:?}"),
    }
    drop(child);
    frontier
        .resume(&fixture.authority, &fixture.font_authority, ticket)
        .unwrap();
    drop((frontier, fixture, font));
    assert!(mapping.mapping_drop_completed() && mapping.unmap_succeeded() && !mapping.failed());
    assert_eq!(mapping.mapped_bytes(), 0);
}

#[test]
fn ascii_letter_to_actual_non_ascii_uses_only_simple_gcb_certificate_and_full_glyph_oracle() {
    let text = "abcéà".repeat(67);
    let mut fixture = Fixture::new(&text, independent_font());
    ready_breaks(&mut fixture);
    let range = 0..fixture.source.utf16_len();
    let mut frontier = begin(&fixture, range.clone(), options(&fixture, false));
    let mut oracle = full(&fixture, range.clone(), options(&fixture, false));
    let mut covered = 0;
    while covered < range.end {
        let (part, work) = first(&mut fixture, &mut frontier, 1, 1);
        let (mut child, ticket) = part.into_parts();
        assert_eq!(child.source_range().start, covered);
        assert!(work.source > 0 && work.shapes == 1);
        compare(&fixture, &mut child, &mut oracle);
        covered = child.source_range().end;
        let resources = child.resource_observer();
        drop(child);
        assert_eq!(resources.snapshot().files_open, 0);
        frontier
            .resume(&fixture.authority, &fixture.font_authority, ticket)
            .unwrap();
    }
}

#[test]
fn ascii_letter_hidden_tail_and_non_simple_plan_keep_original_rejection_paths() {
    for hidden in ['\u{ad}', '\u{2060}'] {
        let text = format!("{}{hidden}", "A".repeat(600));
        let mut fixture = Fixture::new(&text, independent_font());
        ready_breaks(&mut fixture);
        let mut frontier = begin(
            &fixture,
            0..fixture.source.utf16_len(),
            options(&fixture, false),
        );
        let mut rejected = false;
        for _ in 0..4096 {
            let step = frontier
                .step(
                    &fixture.authority,
                    &fixture.font_authority,
                    &mut fixture.breaks,
                    MarkdownShapingBudget::default(),
                    128,
                )
                .unwrap();
            assert_eq!((step.input_bytes, step.shapes), (0, 0));
            assert!(frontier.child.is_none());
            if step.answer != MarkdownShapingFrontierAnswer::Pending {
                assert_eq!(
                    step.answer,
                    MarkdownShapingFrontierAnswer::NeedsFrontier(
                        MarkdownShapingFrontierReason::UncertifiedWord
                    )
                );
                rejected = true;
                break;
            }
        }
        assert!(rejected); // 字母领头不借Common标点修复绕过原whole-source SHY/DI门。
    }
    let actual = MarkdownLayoutFont::resolve(Arc::new(ROBOTO), "Roboto-Regular", &[]).unwrap();
    let mut fixture = Fixture::new("(é中文)", actual);
    ready_breaks(&mut fixture);
    let mut frontier = begin(
        &fixture,
        0..fixture.source.utf16_len(),
        options(&fixture, false),
    );
    for _ in 0..4096 {
        let step = frontier
            .step(
                &fixture.authority,
                &fixture.font_authority,
                &mut fixture.breaks,
                MarkdownShapingBudget::default(),
                128,
            )
            .unwrap();
        assert_eq!((step.input_bytes, step.shapes), (0, 0));
        assert!(frontier.child.is_none());
        if step.answer != MarkdownShapingFrontierAnswer::Pending {
            assert_eq!(
                step.answer,
                MarkdownShapingFrontierAnswer::NeedsFrontier(
                    MarkdownShapingFrontierReason::NonAscii
                )
            );
            return;
        }
    }
    panic!("真实GPOS/连字计划不能因Common标点领头变成simpleGCB")
}

#[test]
#[ignore = "需要真实Native字体回退抓取的PingFang源；设置OMNI_FRONTIER_CJK_FONT"]
fn actual_cjk_ascii_common_prefix_uses_same_plan_and_matches_complete_oracle() {
    let (font, mapping) = ping_fang();
    for prefix in ["(", " ", "!?[ "] {
        let text = format!("前🙂 {prefix}{})", "中文𠮷，汉字。".repeat(100));
        let mut fixture = Fixture::new(&text, font.clone());
        ready_breaks(&mut fixture);
        let range = 4..fixture.source.utf16_len();
        let mut frontier = begin(&fixture, range.clone(), hani_options());
        let mut oracle = full(&fixture, range.clone(), hani_options());
        let mut covered = range.start;
        let mut parts = 0;
        while covered < range.end {
            let (part, _) = first(&mut fixture, &mut frontier, 7, 1);
            let (mut child, ticket) = part.into_parts();
            assert_eq!(child.source_range().start, covered);
            compare(&fixture, &mut child, &mut oracle);
            assert!(
                child
                    .frontier_word_end(&fixture.authority, &fixture.font_authority)
                    .unwrap()
                    .is_none()
            );
            covered = child.source_range().end;
            let observer = child.resource_observer();
            drop(child);
            assert_eq!(observer.snapshot().files_open, 0);
            assert_eq!(observer.snapshot().mappings_open, 0);
            frontier
                .resume(&fixture.authority, &fixture.font_authority, ticket)
                .unwrap();
            parts += 1;
        }
        eprintln!(
            "cjk_actual_ascii_common prefix={prefix:?} parts={parts} end={covered} every_glyph_exact=true"
        );
        assert_eq!(covered, range.end);
    }
    drop(font);
    assert!(mapping.mapping_drop_completed() && mapping.unmap_succeeded() && !mapping.failed());
    assert_eq!(mapping.mapped_bytes(), 0);
    assert!(mapping.source_descriptor_closed() && mapping.backing_descriptor_closed());
}
