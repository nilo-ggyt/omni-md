//! Complete glyph oracles are confined to finite synthetic inputs. Production measurement
//! retains neither a copied document nor an array of all glyphs.
use super::*;
use crate::{
    MarkdownDocumentBuffer, MarkdownGlyphClusterStatus, MarkdownLayoutFont,
    MarkdownLineBreakBudget, MarkdownShapingBudget, MarkdownShapingOptions, MarkdownShapingStatus,
};

const ROBOTO: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../vendor/harfrust/benches/fonts/Roboto-Regular.ttf"
));

// Keep the real temporary document, captured source, font-backed shaping job, and indexes
// together so each test exercises the same ownership chain as production measurement.
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
        // Capture a real isolated file before creating any layout authority. Equal bytes in
        // another fixture therefore do not acquire this capture's source or job identity.
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
                // This fixture range starts at the document origin or immediately after LF.
                // Only the actual source end supplies the end-of-text shaping flag.
                beginning_of_text: range.start == 0
                    || text.encode_utf16().nth(range.start - 1) == Some(10),
                end_of_text: range.end == source.utf16_len(),
                features: Vec::new(),
            },
        )
        .unwrap();
        // Feed the original source through bounded ingestion before asking the actual font
        // shaper for glyphs; the test does not substitute character-count width estimates.
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
        // Finish the real cluster and Unicode break indexes separately from measurement.
        // Tests that intentionally need PendingDependencies call measure before this step.
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
        // Assert each public work dimension against its own quantum. These bounds describe
        // algorithmic work, not elapsed time, resident memory, or OS resource release.
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
            // Only Selected supplies a choice. A yield must advance work, and unexpected
            // dependency waits cannot be accepted as a successfully measured line.
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
        // Read original glyph advances independently of the cluster-prefix index under
        // test, keeping this finite oracle separate from the measurement accumulator.
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
        advance
    }
}

/// Shape an adjacent item from the same capture with its actual size and direction.
/// No oracle glyphs are copied into the new job or its cluster index.
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
    // The finite oracle reads the new job's real font output without consuming any
    // accumulated width from the line measurer.
    let mut width = 0;
    for index in 0..shape.profile().unwrap().glyph_count {
        let glyph = shape.glyph(source, font, index).unwrap().unwrap();
        if range.contains(&glyph.source_utf16_cluster) {
            width += i64::from(glyph.x_advance);
        }
    }
    width
}

#[test]
fn adjacent_font_and_direction_items_keep_one_real_prefix_and_boundary() {
    // Three actual items share one source capture but change direction and font size.
    // Their measured prefix must survive both handoffs in original source coordinates.
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
    // Exhausting the first item requests its real successor; it is not document EOF.
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
    // The third item supplies the successor flags and overflow needed to select the
    // complete prefix of the first two items without losing their accumulated width.
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
    // Identical text in a separately captured file cannot continue the original line.
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
    // Rejection consumes the continuation attempt; restoring the original provider must
    // not revive the failed measurement.
    assert!(matches!(
        measure.step(
            &mut fixture.shape,
            &mut fixture.clusters,
            &mut fixture.breaks,
            MarkdownSoftLineMeasureBudget::default()
        ),
        Err(MarkdownSoftLineMeasureError::Poisoned)
    ));
}

#[test]
fn continuation_requires_adjacent_complete_item_and_original_previous_job() {
    // Width accumulation may cross neither a gap nor an overlap in verified coverage.
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
        ));
    }
    let text = "one two three";
    let mut fixture = Fixture::new(text, 0..3, MarkdownShapingDirection::LeftToRight);
    fixture.dependencies();
    let mut measure = fixture.measure(0, 100_000, MarkdownSoftLineMeasureCancellation::new());
    fixture.finish(&mut measure, MarkdownSoftLineMeasureBudget::default());
    // Matching source, range, and font facts do not replace the particular previous job
    // whose glyphs were consumed by this measurement.
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
    ));
}

#[test]
fn real_font_ltr_rtl_widths_and_tiny_quantums_match_original_glyphs() {
    // Both visual directions and single-operation quanta must select the same original
    // space boundary and signed width as an independent read of the actual font output.
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
    // A paragraph after LF retains global UTF-16 coordinates while its real indexes
    // are pending; completing those dependencies must not restart it at zero.
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
    // The shaped prefix stops before source EOF, so spare width still requires the next
    // item rather than authorizing a completed line.
    let text = "one two three";
    let mut fixture = Fixture::new(text, 0..4, MarkdownShapingDirection::LeftToRight);
    fixture.dependencies();
    let expected_width = fixture.width(0..4);
    let mut measure = fixture.measure(0, i64::MAX, MarkdownSoftLineMeasureCancellation::new());
    assert!(
        matches!(fixture.finish(&mut measure, MarkdownSoftLineMeasureBudget::default()),
        MarkdownSoftLineMeasureChoice::NeedNextItem { source_utf16, x_advance_26_6, .. } if source_utf16 == (0..4) && x_advance_26_6 == expected_width)
    );

    // Emergency breaking, missing glyphs, and dictionary segmentation each remain an
    // explicit unfinished stage instead of being reported as an ordinary measured line.
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
    // First cache a genuine selection, then fail reclaim of an actual original-source
    // read window. Neither cancellation nor that cached choice may hide the source EIO.
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
    // A second capture of the same document is still foreign authority. Rejection closes
    // the break job's real backing resources and leaves no usable old index.
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
    // Cancellation remains the public outcome on repeated advances of the same job.
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
    // This finite 80 KiB source is fixture data; production does not build this String.
    let text = "word ".repeat(16_384);
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
    // Observe owned index buffers and mappings independently. Their byte accounting is
    // not a measurement of whole-process RSS.
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
    // The measurement still owns the captured source after the fixture is dropped.
    // Its explicit close must release that last owner and complete the real unmap.
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
    // The original shaper, font, and source owners have now actually been dropped;
    // the measurement observer alone does not certify their independent release.
    assert!(shape.resources_released());
}

#[test]
fn empty_invalid_width_and_low_surrogate_never_create_fake_boundaries() {
    // Empty input has its own result. Invalid widths and a start inside a surrogate
    // pair cannot manufacture a valid source boundary.
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
    // A different shaping job cannot replace the captured job, and a subsequent call
    // with the original provider cannot revive the poisoned measurement.
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

    // A real source reclaim failure takes precedence over the later foreign-identity
    // check, while the break index's owned files, mappings, and buffers are released.
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
    // The previous provider borrow has ended. Cancellation during this store handoff
    // must be checked before the continuation can replace the font authority.
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
    // A permit requires an actually measured item and that item's original job.
    // Equal source coverage from another shaping job grants no measured-item authority.
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
    }; // The temporary permit retains no job; only a weak line owner and revocation flag remain.
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
    // Continuation preserves this one line scope, but destroying its owner revokes
    // the outstanding receipt without allowing it to keep the measurement alive.
    drop(measure);
    assert!(!line.alive.load(Ordering::Acquire));
    assert!(line.owner.upgrade().is_none());
}
