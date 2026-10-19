//! 完整字形oracle仅用于有限合成输入；生产测量不复制文本或字形数组。
use super::*;
use crate::{
    MarkdownDocumentBuffer, MarkdownGlyphClusterStatus, MarkdownLayoutFont,
    MarkdownLineBreakBudget, MarkdownShapingBudget, MarkdownShapingOptions, MarkdownShapingStatus,
};

const ROBOTO: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../vendor/harfrust/benches/fonts/Roboto-Regular.ttf"
));

struct Fixture {
    _directory: tempfile::TempDir,
    document: MarkdownDocumentBuffer,
    source: MarkdownLayoutTextSource,
    authority: MarkdownLayoutTextAuthority,
    font: MarkdownLayoutFontAuthority,
    shape: MarkdownShapingJob,
    clusters: MarkdownGlyphClusterIndex,
    breaks: MarkdownLayoutBreakJob,
}
impl Fixture {
    fn new(text: &str, range: Range<usize>, direction: MarkdownShapingDirection) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("软行有限控制.md");
        std::fs::write(&path, text).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, text.len() as u64 + 65536).unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let authority = source.authority();
        let font = MarkdownLayoutFont::resolve(Arc::new(ROBOTO), "Roboto-Regular", &[]).unwrap();
        let font_authority = font.authority();
        let mut shape = MarkdownShapingJob::new(
            source.clone(),
            &authority,
            font,
            &font_authority,
            range.clone(),
            MarkdownShapingOptions {
                direction,
                script: *b"Latn",
                language: Some("en".into()),
                points: 16.0,
                // 此测试range在真实文档起点或明确LF之后开始；末尾仅真实EOF才是EOT。
                beginning_of_text: range.start == 0
                    || text.encode_utf16().nth(range.start - 1) == Some(10),
                end_of_text: range.end == source.utf16_len(),
                features: Vec::new(),
            },
        )
        .unwrap();
        while shape.status() == MarkdownShapingStatus::Ingesting {
            shape
                .step(
                    &authority,
                    &font_authority,
                    MarkdownShapingBudget {
                        bytes: 31,
                        scalars: 7,
                    },
                )
                .unwrap();
        }
        shape.shape(&authority, &font_authority).unwrap();
        let clusters =
            MarkdownGlyphClusterIndex::new(&mut shape, &authority, &font_authority).unwrap();
        let breaks = MarkdownLayoutBreakJob::new(source.clone(), &authority).unwrap();
        Self {
            _directory: directory,
            document,
            source,
            authority,
            font: font_authority,
            shape,
            clusters,
            breaks,
        }
    }
    fn dependencies(&mut self) {
        while self.clusters.status() != MarkdownGlyphClusterStatus::Ready {
            self.clusters.step(&mut self.shape, 17).unwrap();
        }
        for _ in 0..1_000_000 {
            if self
                .breaks
                .step(
                    &self.authority,
                    MarkdownLineBreakBudget {
                        bytes: 37,
                        scalars: 11,
                        records: 9,
                    },
                )
                .unwrap()
                .ready
            {
                return;
            }
        }
        panic!("有限测试依赖未完成");
    }
    fn measure(
        &mut self,
        start: usize,
        width: i64,
        cancellation: MarkdownSoftLineMeasureCancellation,
    ) -> MarkdownSoftLineMeasureJob {
        MarkdownSoftLineMeasureJob::new(
            self.source.clone(),
            &self.authority,
            &self.font,
            &mut self.shape,
            &mut self.breaks,
            MarkdownSoftLineMeasureRequest {
                start_utf16: start,
                width_26_6: width,
                cancellation,
            },
        )
        .unwrap()
    }
    fn finish(
        &mut self,
        measure: &mut MarkdownSoftLineMeasureJob,
        budget: MarkdownSoftLineMeasureBudget,
    ) -> MarkdownSoftLineMeasureChoice {
        for _ in 0..1_000_000 {
            let step = measure
                .step(
                    &mut self.shape,
                    &mut self.clusters,
                    &mut self.breaks,
                    budget,
                )
                .unwrap();
            assert!(
                step.break_queries <= budget.break_queries.min(MARKDOWN_SOFT_LINE_BREAK_QUERIES)
            );
            assert!(step.clusters <= budget.clusters.min(MARKDOWN_SOFT_LINE_CLUSTERS));
            match step.answer {
                MarkdownSoftLineMeasureAnswer::Selected(choice) => return choice,
                MarkdownSoftLineMeasureAnswer::PendingDependencies => panic!("真实依赖已经完成"),
                MarkdownSoftLineMeasureAnswer::Yielded => {
                    assert!(step.break_queries + step.clusters > 0)
                }
            }
        }
        panic!("有限测量未完成");
    }
    fn width(&mut self, range: Range<usize>) -> i64 {
        let count = self.shape.profile().unwrap().glyph_count;
        let mut advance = 0i64;
        for index in 0..count {
            let glyph = self
                .shape
                .glyph(&self.authority, &self.font, index)
                .unwrap()
                .unwrap();
            if range.contains(&glyph.source_utf16_cluster) {
                advance += i64::from(glyph.x_advance);
            }
        }
        advance // 独立读取原真实glyph，不消费被测cluster prefix，也不按字符猜宽。
    }
}

/// 与被测首item共用同一capture；不同字体尺寸/方向沿真实shape任务产生，不复制oracle glyph。
fn following_item(
    fixture: &Fixture,
    range: Range<usize>,
    direction: MarkdownShapingDirection,
    points: f32,
) -> (
    MarkdownLayoutFontAuthority,
    MarkdownShapingJob,
    MarkdownGlyphClusterIndex,
) {
    let font = MarkdownLayoutFont::resolve(Arc::new(ROBOTO), "Roboto-Regular", &[]).unwrap();
    let authority = font.authority();
    let mut shape = MarkdownShapingJob::new(
        fixture.source.clone(),
        &fixture.authority,
        font,
        &authority,
        range.clone(),
        MarkdownShapingOptions {
            direction,
            script: *b"Latn",
            language: Some("en".into()),
            points,
            beginning_of_text: range.start == 0,
            end_of_text: range.end == fixture.source.utf16_len(),
            features: Vec::new(),
        },
    )
    .unwrap();
    while shape.status() == MarkdownShapingStatus::Ingesting {
        shape
            .step(
                &fixture.authority,
                &authority,
                MarkdownShapingBudget::default(),
            )
            .unwrap();
    }
    shape.shape(&fixture.authority, &authority).unwrap();
    let mut clusters =
        MarkdownGlyphClusterIndex::new(&mut shape, &fixture.authority, &authority).unwrap();
    while clusters.status() != MarkdownGlyphClusterStatus::Ready {
        clusters.step(&mut shape, 7).unwrap();
    }
    (authority, shape, clusters)
}

fn actual_advance(
    shape: &mut MarkdownShapingJob,
    source: &MarkdownLayoutTextAuthority,
    font: &MarkdownLayoutFontAuthority,
    range: Range<usize>,
) -> i64 {
    let mut width = 0;
    for index in 0..shape.profile().unwrap().glyph_count {
        let glyph = shape.glyph(source, font, index).unwrap().unwrap();
        if range.contains(&glyph.source_utf16_cluster) {
            width += i64::from(glyph.x_advance);
        }
    }
    width // 有限测试独立读取真实字形，不消费测量器的累计值，也不按字符数量猜宽度。
}

#[test]
fn adjacent_font_and_direction_items_keep_one_real_prefix_and_boundary() {
    let text = "one two three four";
    let mut fixture = Fixture::new(text, 0..3, MarkdownShapingDirection::LeftToRight);
    fixture.dependencies();
    let (font2, mut shape2, mut clusters2) =
        following_item(&fixture, 3..8, MarkdownShapingDirection::RightToLeft, 21.0);
    let (font3, mut shape3, mut clusters3) = following_item(
        &fixture,
        8..text.len(),
        MarkdownShapingDirection::LeftToRight,
        16.0,
    );
    let width = fixture.width(0..3) + actual_advance(&mut shape2, &fixture.authority, &font2, 3..8);
    let mut measure = fixture.measure(0, width, MarkdownSoftLineMeasureCancellation::new());
    let first = fixture.finish(&mut measure, MarkdownSoftLineMeasureBudget::default());
    assert!(
        matches!(first, MarkdownSoftLineMeasureChoice::NeedNextItem { source_utf16, .. } if source_utf16 == (0..3))
    );
    measure
        .continue_item(&mut fixture.shape, &mut shape2, &font2, &mut fixture.breaks)
        .unwrap();
    loop {
        match measure
            .step(
                &mut shape2,
                &mut clusters2,
                &mut fixture.breaks,
                MarkdownSoftLineMeasureBudget {
                    break_queries: 1,
                    clusters: 1,
                },
            )
            .unwrap()
            .answer
        {
            MarkdownSoftLineMeasureAnswer::Selected(
                MarkdownSoftLineMeasureChoice::NeedNextItem {
                    source_utf16,
                    x_advance_26_6,
                    ..
                },
            ) => {
                assert_eq!(source_utf16, 0..8);
                assert_eq!(x_advance_26_6, width);
                break;
            }
            MarkdownSoftLineMeasureAnswer::Yielded => {}
            answer => panic!("两个item接缝必须继续而非伪EOF: {answer:?}"),
        }
    }
    measure
        .continue_item(&mut shape2, &mut shape3, &font3, &mut fixture.breaks)
        .unwrap();
    loop {
        match measure
            .step(
                &mut shape3,
                &mut clusters3,
                &mut fixture.breaks,
                MarkdownSoftLineMeasureBudget {
                    break_queries: 1,
                    clusters: 1,
                },
            )
            .unwrap()
            .answer
        {
            MarkdownSoftLineMeasureAnswer::Selected(
                MarkdownSoftLineMeasureChoice::MeasuredBreak(line),
            ) => {
                assert_eq!(line.source_utf16, 0..8);
                assert_eq!(line.x_advance_26_6, width);
                assert_eq!(line.boundary_flags & UNSAFE_TO_BREAK, 0);
                break;
            }
            MarkdownSoftLineMeasureAnswer::Yielded => {}
            answer => panic!("后项真实flags和溢出后应选原完整前缀: {answer:?}"),
        }
    }
}

#[test]
fn continuation_rejects_same_bytes_foreign_source_and_poison_is_permanent() {
    let text = "one two three";
    let mut fixture = Fixture::new(text, 0..3, MarkdownShapingDirection::LeftToRight);
    fixture.dependencies();
    let mut measure = fixture.measure(0, 100_000, MarkdownSoftLineMeasureCancellation::new());
    fixture.finish(&mut measure, MarkdownSoftLineMeasureBudget::default());
    let mut foreign = Fixture::new(text, 3..text.len(), MarkdownShapingDirection::LeftToRight);
    let error = measure
        .continue_item(
            &mut fixture.shape,
            &mut foreign.shape,
            &foreign.font,
            &mut fixture.breaks,
        )
        .unwrap_err();
    assert!(matches!(
        error,
        MarkdownSoftLineMeasureError::Shaping(MarkdownShapingError::SourceMismatch)
    ));
    assert!(matches!(
        measure.step(
            &mut fixture.shape,
            &mut fixture.clusters,
            &mut fixture.breaks,
            MarkdownSoftLineMeasureBudget::default()
        ),
        Err(MarkdownSoftLineMeasureError::Poisoned)
    )); // 拒绝候选后不能用真正原任务恢复一张已消费的行票。
}

#[test]
fn continuation_requires_adjacent_complete_item_and_original_previous_job() {
    for gap in [true, false] {
        let text = "one two three";
        let mut fixture = Fixture::new(text, 0..3, MarkdownShapingDirection::LeftToRight);
        fixture.dependencies();
        let mut measure = fixture.measure(0, 100_000, MarkdownSoftLineMeasureCancellation::new());
        fixture.finish(&mut measure, MarkdownSoftLineMeasureBudget::default());
        let range = if gap { 4..text.len() } else { 2..text.len() };
        let (font, mut next, _) =
            following_item(&fixture, range, MarkdownShapingDirection::LeftToRight, 16.0);
        assert!(matches!(
            measure.continue_item(&mut fixture.shape, &mut next, &font, &mut fixture.breaks),
            Err(MarkdownSoftLineMeasureError::InvalidRequest)
        )); // 缺口和重叠都不能借宽度累加跨越未验证覆盖。
    }
    let text = "one two three";
    let mut fixture = Fixture::new(text, 0..3, MarkdownShapingDirection::LeftToRight);
    fixture.dependencies();
    let mut measure = fixture.measure(0, 100_000, MarkdownSoftLineMeasureCancellation::new());
    fixture.finish(&mut measure, MarkdownSoftLineMeasureBudget::default());
    let (_, mut impostor, _) =
        following_item(&fixture, 0..3, MarkdownShapingDirection::LeftToRight, 16.0);
    let (font, mut next, _) = following_item(
        &fixture,
        3..text.len(),
        MarkdownShapingDirection::LeftToRight,
        16.0,
    );
    assert!(matches!(
        measure.continue_item(&mut impostor, &mut next, &font, &mut fixture.breaks),
        Err(MarkdownSoftLineMeasureError::JobMismatch)
    )); // 同Source、相同范围和字体仍不是实际已测的原前项。
}

#[test]
fn real_font_ltr_rtl_widths_and_tiny_quantums_match_original_glyphs() {
    let text = "one two three four";
    for direction in [
        MarkdownShapingDirection::LeftToRight,
        MarkdownShapingDirection::RightToLeft,
    ] {
        let mut fixture = Fixture::new(text, 0..text.len(), direction);
        fixture.dependencies();
        let width = fixture.width(0..8);
        for budget in [
            MarkdownSoftLineMeasureBudget::default(),
            MarkdownSoftLineMeasureBudget {
                break_queries: 1,
                clusters: 1,
            },
        ] {
            let mut measure = fixture.measure(0, width, MarkdownSoftLineMeasureCancellation::new());
            let choice = fixture.finish(&mut measure, budget);
            let MarkdownSoftLineMeasureChoice::MeasuredBreak(value) = choice else {
                panic!("真实Roboto空格机会不应丢失: {choice:?}")
            };
            assert_eq!(value.source_utf16, 0..8);
            assert_eq!(value.x_advance_26_6, fixture.width(0..8));
            assert_eq!(value.boundary_flags & UNSAFE_TO_BREAK, 0);
            assert_eq!(value.boundary.utf16, 8);
        }
    }
}

#[test]
fn pending_indexes_and_nonzero_paragraph_keep_original_global_coordinates() {
    let text = "prefix\none two three";
    let mut fixture = Fixture::new(text, 7..text.len(), MarkdownShapingDirection::LeftToRight);
    let width = fixture.width(7..11);
    let mut measure = fixture.measure(7, width, MarkdownSoftLineMeasureCancellation::new());
    let step = measure
        .step(
            &mut fixture.shape,
            &mut fixture.clusters,
            &mut fixture.breaks,
            MarkdownSoftLineMeasureBudget::default(),
        )
        .unwrap();
    assert_eq!(
        step.answer,
        MarkdownSoftLineMeasureAnswer::PendingDependencies
    );
    fixture.dependencies();
    let choice = fixture.finish(
        &mut measure,
        MarkdownSoftLineMeasureBudget {
            break_queries: 1,
            clusters: 1,
        },
    );
    assert!(
        matches!(choice, MarkdownSoftLineMeasureChoice::MeasuredBreak(value) if value.source_utf16 == (7..11) && value.x_advance_26_6 == width)
    );
}

#[test]
fn incomplete_item_missing_dictionary_and_emergency_are_never_complete_lines() {
    let text = "one two three";
    let mut fixture = Fixture::new(text, 0..4, MarkdownShapingDirection::LeftToRight);
    fixture.dependencies();
    let expected_width = fixture.width(0..4);
    let mut measure = fixture.measure(0, i64::MAX, MarkdownSoftLineMeasureCancellation::new());
    assert!(
        matches!(fixture.finish(&mut measure, MarkdownSoftLineMeasureBudget::default()),
        MarkdownSoftLineMeasureChoice::NeedNextItem { source_utf16, x_advance_26_6, .. } if source_utf16 == (0..4) && x_advance_26_6 == expected_width)
    );

    for (text, expected) in [("longword", 0), ("\u{e000}", 1), ("\u{0e01}\u{0e02}", 2)] {
        let mut fixture = Fixture::new(
            text,
            0..text.encode_utf16().count(),
            MarkdownShapingDirection::LeftToRight,
        );
        fixture.dependencies();
        let mut measure = fixture.measure(
            0,
            if expected == 0 { 1 } else { i64::MAX },
            MarkdownSoftLineMeasureCancellation::new(),
        );
        let choice = fixture.finish(&mut measure, MarkdownSoftLineMeasureBudget::default());
        assert!(
            match expected {
                0 => matches!(
                    choice,
                    MarkdownSoftLineMeasureChoice::NeedsEmergencyBreak(_)
                ),
                1 =>
                    matches!(choice, MarkdownSoftLineMeasureChoice::NeedsFontFallback { missing_glyphs, .. } if missing_glyphs > 0),
                2 => matches!(choice, MarkdownSoftLineMeasureChoice::NeedsDictionary(_)),
                _ => unreachable!(),
            },
            "未完成阶段被误签: {text:?} {choice:?}"
        );
    }
}

#[test]
fn source_failure_overrides_cancel_and_cached_selection_after_real_eviction() {
    let mut fixture = Fixture::new("one two", 0..7, MarkdownShapingDirection::LeftToRight);
    fixture.dependencies();
    let cancellation = MarkdownSoftLineMeasureCancellation::new();
    let mut measure = fixture.measure(0, i64::MAX, cancellation.clone());
    assert!(matches!(
        fixture.finish(&mut measure, MarkdownSoftLineMeasureBudget::default()),
        MarkdownSoftLineMeasureChoice::MeasuredBreak(_)
    ));
    fixture.source.visit_byte_range(0..1, |_| Ok(())).unwrap();
    fixture.document.fail_next_original_reclaim_for_test();
    assert_eq!(
        fixture.document.reclaim_original_read_windows_for_test(),
        Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
            libc::EIO
        ))
    );
    cancellation.cancel();
    let result = measure.step(
        &mut fixture.shape,
        &mut fixture.clusters,
        &mut fixture.breaks,
        MarkdownSoftLineMeasureBudget::default(),
    );
    assert!(matches!(
        result,
        Err(MarkdownSoftLineMeasureError::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        )) | Err(MarkdownSoftLineMeasureError::Shaping(
            MarkdownShapingError::Source(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                libc::EIO
            ))
        ))
    ));
}

#[test]
fn foreign_break_source_releases_old_indexes_and_cancel_is_permanent() {
    let mut fixture = Fixture::new("one two", 0..7, MarkdownShapingDirection::LeftToRight);
    fixture.dependencies();
    let foreign = MarkdownLayoutTextSource::capture(&fixture.document);
    let mut breaks = MarkdownLayoutBreakJob::new(foreign.clone(), &foreign.authority()).unwrap();
    let observer = breaks.resource_observer();
    assert!(matches!(
        breaks.require_source(&fixture.authority),
        Err(MarkdownLayoutBreakError::SourceMismatch)
    ));
    assert!(matches!(
        breaks.require_source(&foreign.authority()),
        Err(MarkdownLayoutBreakError::Poisoned)
    ));
    let (grapheme, line) = observer.snapshots();
    assert!(!grapheme.metadata_file_open && !line.metadata_file_open);
    assert_eq!(grapheme.metadata_map_bytes + line.metadata_map_bytes, 0);
    assert_eq!(
        grapheme.metadata_buffer_bytes + line.metadata_buffer_bytes,
        0
    );
    let cancellation = MarkdownSoftLineMeasureCancellation::new();
    let mut measure = fixture.measure(0, i64::MAX, cancellation.clone());
    cancellation.cancel();
    for _ in 0..2 {
        assert!(matches!(
            measure.step(
                &mut fixture.shape,
                &mut fixture.clusters,
                &mut fixture.breaks,
                MarkdownSoftLineMeasureBudget::default()
            ),
            Err(MarkdownSoftLineMeasureError::Cancelled)
        ));
    }
}

#[test]
fn long_original_run_uses_fixed_indexes_and_actual_last_owner_release() {
    let text = "word ".repeat(16_384); // 80KiB有限测试源；产品不会建立此String。
    let mut fixture = Fixture::new(&text, 0..text.len(), MarkdownShapingDirection::LeftToRight);
    fixture.dependencies();
    let mut measure = fixture.measure(0, 16 * 64 * 20, MarkdownSoftLineMeasureCancellation::new());
    assert!(matches!(
        fixture.finish(
            &mut measure,
            MarkdownSoftLineMeasureBudget {
                break_queries: 1,
                clusters: 1
            }
        ),
        MarkdownSoftLineMeasureChoice::MeasuredBreak(_)
    ));
    let cluster = fixture.clusters.resource_observer();
    let breaks = fixture.breaks.resource_observer();
    let shape = fixture.shape.resource_observer();
    let source = measure.resource_observer();
    let mapping = fixture.document.original_backing_observer().unwrap();
    assert_eq!(cluster.snapshot().peak_mapped_bytes, 0);
    assert_eq!(cluster.snapshot().peak_buffer_bytes, 16 * 1024);
    let (gcb, lb) = breaks.snapshots();
    assert_eq!(gcb.metadata_peak_map_bytes + lb.metadata_peak_map_bytes, 0);
    assert!(
        gcb.metadata_peak_buffer_bytes <= 16 * 1024 && lb.metadata_peak_buffer_bytes <= 16 * 1024
    );
    drop(fixture);
    assert!(!source.payload_released() && !mapping.unmap_succeeded());
    measure.close();
    assert!(source.payload_released() && mapping.unmap_succeeded() && !mapping.failed());
    assert_eq!(mapping.mapped_bytes(), 0);
    assert!(
        !cluster.snapshot().file_open
            && cluster.snapshot().mapped_bytes == 0
            && cluster.snapshot().buffer_bytes == 0
            && !cluster.snapshot().cleanup_failed
    );
    let (gcb, lb) = breaks.snapshots();
    assert!(
        !gcb.metadata_file_open
            && !lb.metadata_file_open
            && gcb.metadata_map_bytes == 0
            && lb.metadata_map_bytes == 0
            && gcb.metadata_buffer_bytes == 0
            && lb.metadata_buffer_bytes == 0
    );
    assert!(shape.resources_released()); // 真实shaper/Font/Source最后字段析构，不是测量器单独签全部释放。
}

#[test]
fn empty_invalid_width_and_low_surrogate_never_create_fake_boundaries() {
    let mut empty = Fixture::new("", 0..0, MarkdownShapingDirection::LeftToRight);
    empty.dependencies();
    let mut measure = empty.measure(0, 1, MarkdownSoftLineMeasureCancellation::new());
    assert_eq!(
        empty.finish(&mut measure, MarkdownSoftLineMeasureBudget::default()),
        MarkdownSoftLineMeasureChoice::EmptySource
    );
    let mut fixture = Fixture::new("a🙂", 0..3, MarkdownShapingDirection::LeftToRight);
    for (start_utf16, width_26_6) in [(0, 0), (2, 1)] {
        assert!(
            MarkdownSoftLineMeasureJob::new(
                fixture.source.clone(),
                &fixture.authority,
                &fixture.font,
                &mut fixture.shape,
                &mut fixture.breaks,
                MarkdownSoftLineMeasureRequest {
                    start_utf16,
                    width_26_6,
                    cancellation: MarkdownSoftLineMeasureCancellation::new()
                }
            )
            .is_err()
        );
    }
}

#[test]
fn another_shape_job_cannot_resign_a_failed_measure_and_break_health_wins_identity() {
    let mut original = Fixture::new("one two", 0..7, MarkdownShapingDirection::LeftToRight);
    original.dependencies();
    let mut measure = original.measure(0, i64::MAX, MarkdownSoftLineMeasureCancellation::new());
    let mut another = Fixture::new("one two", 0..7, MarkdownShapingDirection::LeftToRight);
    assert!(matches!(
        measure.step(
            &mut another.shape,
            &mut original.clusters,
            &mut original.breaks,
            MarkdownSoftLineMeasureBudget::default()
        ),
        Err(MarkdownSoftLineMeasureError::JobMismatch)
    ));
    assert!(matches!(
        measure.step(
            &mut original.shape,
            &mut original.clusters,
            &mut original.breaks,
            MarkdownSoftLineMeasureBudget::default()
        ),
        Err(MarkdownSoftLineMeasureError::Poisoned)
    ));

    original.source.visit_byte_range(0..1, |_| Ok(())).unwrap();
    original.document.fail_next_original_reclaim_for_test();
    assert_eq!(
        original.document.reclaim_original_read_windows_for_test(),
        Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
            libc::EIO
        ))
    );
    let observer = original.breaks.resource_observer();
    assert!(matches!(
        original.breaks.require_source(&another.authority),
        Err(MarkdownLayoutBreakError::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
    let (gcb, lb) = observer.snapshots();
    assert!(
        !gcb.metadata_file_open
            && !lb.metadata_file_open
            && gcb.metadata_map_bytes == 0
            && lb.metadata_map_bytes == 0
            && gcb.metadata_buffer_bytes == 0
            && lb.metadata_buffer_bytes == 0
    );
}

#[test]
fn prepared_continuation_checks_cancel_after_old_provider_borrow_has_ended() {
    let mut fixture = Fixture::new("one two", 0..3, MarkdownShapingDirection::LeftToRight);
    fixture.dependencies();
    let (next_font, mut next, _) =
        following_item(&fixture, 3..7, MarkdownShapingDirection::LeftToRight, 16.0);
    let cancellation = MarkdownSoftLineMeasureCancellation::new();
    let mut measure = fixture.measure(0, i64::MAX, cancellation.clone());
    assert!(matches!(
        fixture.finish(&mut measure, MarkdownSoftLineMeasureBudget::default()),
        MarkdownSoftLineMeasureChoice::NeedNextItem { .. }
    ));
    let continuation = measure
        .prepare_continue(&mut JobGlyphProvider::new(&mut fixture.shape, None))
        .unwrap();
    // 前项借用已经结束。模拟真正跨Store借用交接期间的取消，不能先换Font后才检查。
    cancellation.cancel();
    assert!(matches!(
        continuation.continue_with(
            &mut JobGlyphProvider::new(&mut next, None),
            &next_font,
            &mut fixture.breaks
        ),
        Err(MarkdownSoftLineMeasureError::Cancelled)
    ));
    assert!(measure.payload.is_none());
}

#[cfg(feature = "paged-safe-glyph-slices")]
#[test]
fn measured_item_permit_requires_real_stage_and_original_job_and_keeps_only_one_line_scope() {
    let mut fixture = Fixture::new("one two", 0..3, MarkdownShapingDirection::LeftToRight);
    fixture.dependencies();
    let (next_font, mut next, mut next_index) =
        following_item(&fixture, 3..7, MarkdownShapingDirection::LeftToRight, 16.0);
    let (_, mut impostor, _) =
        following_item(&fixture, 0..3, MarkdownShapingDirection::LeftToRight, 16.0);
    let mut measure = fixture.measure(0, i64::MAX, MarkdownSoftLineMeasureCancellation::new());
    assert!(matches!(
        measure.measured_item_permit(&mut JobGlyphProvider::new(&mut fixture.shape, None)),
        Err(MarkdownSoftLineMeasureError::InvalidRequest)
    ));
    fixture.finish(&mut measure, MarkdownSoftLineMeasureBudget::default());
    let line = {
        let permit = measure
            .measured_item_permit(&mut JobGlyphProvider::new(&mut fixture.shape, None))
            .unwrap();
        assert!(matches!(
            permit.require(&mut JobGlyphProvider::new(&mut impostor, None)),
            Err(MarkdownSoftLineMeasureError::JobMismatch)
        ));
        let facts = permit
            .require(&mut JobGlyphProvider::new(&mut fixture.shape, None))
            .unwrap();
        assert_eq!(facts.measured_utf16, 0..3);
        facts.line
    }; // 临时许可没有保存Job；借用结束后只留下固定整行弱身份与撤权原子。
    let continuation = measure
        .prepare_continue(&mut JobGlyphProvider::new(&mut fixture.shape, None))
        .unwrap();
    continuation
        .continue_with(
            &mut JobGlyphProvider::new(&mut next, None),
            &next_font,
            &mut fixture.breaks,
        )
        .unwrap();
    assert!(line.alive.load(Ordering::Acquire));
    assert!(Weak::ptr_eq(
        &line.owner,
        &Arc::downgrade(&measure.identity)
    ));
    loop {
        if matches!(
            measure
                .step(
                    &mut next,
                    &mut next_index,
                    &mut fixture.breaks,
                    MarkdownSoftLineMeasureBudget::default()
                )
                .unwrap()
                .answer,
            MarkdownSoftLineMeasureAnswer::Selected(_)
        ) {
            break;
        }
    }
    drop(measure);
    assert!(!line.alive.load(Ordering::Acquire));
    assert!(line.owner.upgrade().is_none());
}
