//! Real Style→Frontier→Store→Measure composition. Pending line-break dependencies
//! advance under a later original quantum without replacing public line outcomes.
use super::*;
use crate::{MarkdownDocumentBuffer, MarkdownLayoutFont};

struct Fixture {
    _directory: tempfile::TempDir,
    document: MarkdownDocumentBuffer,
    source: MarkdownLayoutTextSource,
}
impl Fixture {
    fn new(text: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("按需断行.md");
        std::fs::write(&path, text).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, text.len() as u64 + 65536).unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        Self {
            _directory: directory,
            document,
            source,
        }
    }
    fn worker(
        &self,
        font: &MarkdownLayoutFont,
        frontier: bool,
        cancellation: MarkdownLineComposeCancellation,
    ) -> (MarkdownLineComposer, MarkdownLineRegisteredFont) {
        let catalog = MarkdownLineFontCatalog::new();
        let registered = catalog.register(font).unwrap();
        let constructor = if frontier {
            MarkdownLineComposer::new_with_frontier
        } else {
            MarkdownLineComposer::new
        };
        let worker = constructor(
            self.source.clone(),
            &self.source.authority(),
            catalog,
            80 * 64,
            cancellation,
        )
        .unwrap();
        (worker, registered)
    }
}

#[cfg(target_os = "macos")]
fn font() -> MarkdownLayoutFont {
    let bytes = crate::MarkdownLayoutFontBytes::capture_file(
        std::path::Path::new("/System/Library/Fonts/SFNSMono.ttf"),
        32 * 1024 * 1024,
    )
    .unwrap();
    MarkdownLayoutFont::resolve(bytes.blob(), ".SFNSMono-Regular", &[]).unwrap()
}

fn style() -> MarkdownLineComposeStyle {
    MarkdownLineComposeStyle {
        presentation: Default::default(),
        points: 16.0,
        language: None,
        features: Vec::new(),
    }
}

fn respond(
    worker: &mut MarkdownLineComposer,
    request: MarkdownLineStyleRequest,
    font: &MarkdownLineRegisteredFont,
) {
    let range = request.source_range();
    worker
        .provide_style(request.respond_registered(range, font, style()).unwrap())
        .unwrap();
}

#[cfg(target_os = "macos")]
#[test]
fn same_source_style_reset_replaces_real_glyph_output_and_closes_old_readers() {
    // Complete real small-font layout, then change visibility/point size. Validate
    // actual glyph output, wrapping, and closed FDs instead of style-value snapshots.
    let fixture = Fixture::new("hello αλφα world");
    let catalog = MarkdownLineFontCatalog::new();
    let registered = catalog.register(&font()).unwrap();
    let mut worker = MarkdownLineComposer::new_with_frontier(
        fixture.source.clone(),
        &fixture.source.authority(),
        catalog.clone(),
        80 * 64,
        MarkdownLineComposeCancellation::new(),
    )
    .unwrap();
    let complete = |worker: &mut MarkdownLineComposer, points: f32| {
        for _ in 0..100_000 {
            let step = worker
                .step(MarkdownLineComposeBudget {
                    bytes: 1,
                    scalars: 1,
                    records: 1,
                    metadata_units: 1,
                })
                .unwrap();
            assert!(
                step.bytes <= 1
                    && step.scalars <= 1
                    && step.records <= 1
                    && step.metadata_units <= 1
                    && step.shapes <= 1
            );
            match step.answer {
                MarkdownLineComposeAnswer::NeedStyle(request) => {
                    let range = request.source_range();
                    worker
                        .provide_style(
                            request
                                .respond_registered(
                                    range,
                                    &registered,
                                    MarkdownLineComposeStyle {
                                        presentation: if points == 0.1 {
                                            crate::MarkdownPresentationStyles::HIDDEN_MARKER
                                        } else {
                                            crate::MarkdownPresentationStyles::UNDERLINE.union(
                                                crate::MarkdownPresentationStyles::STRIKETHROUGH,
                                            )
                                        },
                                        points,
                                        language: None,
                                        features: Vec::new(),
                                    },
                                )
                                .unwrap(),
                        )
                        .unwrap();
                }
                MarkdownLineComposeAnswer::Complete => return,
                MarkdownLineComposeAnswer::Yielded | MarkdownLineComposeAnswer::LineReady(_) => {}
                MarkdownLineComposeAnswer::NeedsResolution(_) => {} // The original resolver resumes next quantum.
                other => panic!(
                    "有限无控制源的真实样式重排不能丢功能：{:?}",
                    std::mem::discriminant(&other)
                ),
            }
        }
        panic!("有限样式重排没有交付真实Complete");
    };
    complete(&mut worker, 0.1);
    let old_writer = worker.resource_observer().unwrap();
    let mut old = worker.publish_ready_prefix().unwrap().unwrap();
    let old_reader = old.resource_observer();
    let line = old.last_line().unwrap().unwrap();
    assert_eq!(line.facts().source_utf16, 0..fixture.source.utf16_len());
    assert!(old.previous_line(&line).unwrap().is_none()); // The actual small-font output has one full line.
    assert_eq!(old.part(&line, 0).unwrap().unwrap().facts().points, 0.1);
    old.close().unwrap(); // Release the actual old prefix before another glyph graph exists.
    worker.reset_styles(catalog).unwrap();
    assert!(worker.source_authority() == &fixture.source.authority());
    assert!(!old_writer.snapshot().file_open && !old_reader.snapshot().file_open);
    // The public continuation must now produce complete new geometry while its
    // former writer/reader have actually closed, rather than retain the old output.
    complete(&mut worker, 24.0);
    let mut output = worker.into_output().unwrap();
    let mut line = output.last_line().unwrap();
    let mut start = fixture.source.utf16_len();
    let mut count = 0;
    while let Some(actual) = line {
        assert_eq!(actual.facts().source_utf16.end, start);
        assert!(actual.facts().source_utf16.start < start);
        assert!(actual.facts().x_advance_26_6 <= 80 * 64); // Every actual fixture grapheme fits this width.
        for index in 0..actual.facts().parts {
            let part = output.part(&actual, index).unwrap().unwrap();
            assert_eq!(part.facts().points, 24.0);
            assert_eq!(
                part.facts().presentation,
                crate::MarkdownPresentationStyles::UNDERLINE
                    .union(crate::MarkdownPresentationStyles::STRIKETHROUGH)
            );
            assert!(part.facts().glyphs > 0);
        }
        start = actual.facts().source_utf16.start;
        count += 1;
        line = output.previous_line(&actual).unwrap();
    }
    assert_eq!(start, 0); // New output must cover the entire original canonical Source.
    assert!(count > 1); // Larger actual font size must replace the old single-line geometry.
    output.close().unwrap();
}

#[cfg(target_os = "macos")]
#[test]
fn first_line_output_and_public_work_are_equal_for_4k_8k_4m() {
    let font = font();
    let mut results = Vec::new(); // Three fixed oracle results, never a product history cache.
    for length in [4096usize, 8192, 4 * 1024 * 1024] {
        let mut text = "hello world ".repeat(length.div_ceil(12));
        text.truncate(length);
        let fixture = Fixture::new(&text);
        let backing = fixture.source.original_backing_observer().unwrap();
        let source_observer = fixture.source.resource_observer();
        let (mut worker, registered) =
            fixture.worker(&font, true, MarkdownLineComposeCancellation::new());
        let resources = worker.worker_resource_observer();
        let (mut bytes, mut first) = (0, None);
        for _ in 0..100_000 {
            let progress = worker.step(MarkdownLineComposeBudget::default()).unwrap();
            assert!(
                progress.bytes <= 4096
                    && progress.scalars <= 256
                    && progress.records <= 256
                    && progress.shapes <= 1
            );
            bytes += progress.bytes;
            match progress.answer {
                MarkdownLineComposeAnswer::NeedStyle(request) => {
                    respond(&mut worker, request, &registered);
                }
                MarkdownLineComposeAnswer::LineReady(line) => {
                    first = Some(line);
                    break;
                }
                MarkdownLineComposeAnswer::Yielded => {}
                _ => panic!("真实自然机会首行不能转为假EOF或未认证解析"),
            }
        }
        let first = first.expect("有限前沿必须实际发布LineEnd");
        // Compare public step work and actual committed first-line coverage across
        // large Sources, then release real output/backing rather than inspect a
        // private line-break read counter.
        results.push((
            bytes,
            first.source_utf16.end,
            worker.frontier_work().unwrap(),
        ));
        let output = worker.into_output().unwrap();
        assert!(output.source_range().end < length);
        output.close().unwrap();
        drop((registered, fixture));
        assert!(source_observer.payload_released() && resources.is_released());
        assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
    }
    assert!(results.windows(2).all(|pair| pair[0] == pair[1]));
    eprintln!("按需LB真实首行 work={results:?} whole_product_pass=false");
}

#[cfg(target_os = "macos")]
fn complete(
    fixture: &Fixture,
    font: &MarkdownLayoutFont,
    frontier: bool,
) -> Vec<(MarkdownComposedLineFacts, Vec<crate::MarkdownShapedGlyph>)> {
    let (mut worker, registered) =
        fixture.worker(font, frontier, MarkdownLineComposeCancellation::new());
    let mut finished = false;
    for _ in 0..200_000 {
        let progress = worker.step(MarkdownLineComposeBudget::default()).unwrap();
        match progress.answer {
            MarkdownLineComposeAnswer::NeedStyle(request) => {
                respond(&mut worker, request, &registered)
            }
            MarkdownLineComposeAnswer::Yielded | MarkdownLineComposeAnswer::LineReady(_) => {}
            MarkdownLineComposeAnswer::Complete => {
                finished = true;
                break;
            }
            _ => panic!("有限safe原字体不应丢功能或伪造边界"),
        }
    }
    assert!(finished);
    let mut output = worker.into_output().unwrap();
    let observer = output.resource_observer();
    let mut line = output.last_line().unwrap();
    let mut actual = Vec::new(); // Only this finite complete oracle retains line results.
    while let Some(current) = line {
        let mut glyphs = Vec::new();
        for index in 0..current.facts().parts {
            let part = output.part(&current, index).unwrap().unwrap();
            for ordinal in 0..part.facts().glyphs {
                let mut glyph = [crate::MarkdownShapedGlyph {
                    glyph_id: 0,
                    source_utf16_cluster: 0,
                    flags: 0,
                    x_advance: 0,
                    y_advance: 0,
                    x_offset: 0,
                    y_offset: 0,
                }];
                output
                    .read_batch(&part, ordinal..ordinal + 1, &mut glyph)
                    .unwrap();
                glyphs.push(glyph[0]);
            }
        }
        actual.push((current.facts().clone(), glyphs));
        line = output.previous_line(&current).unwrap();
    }
    output.close().unwrap();
    assert!(
        !observer.snapshot().file_open
            && observer.snapshot().mapped_bytes == 0
            && !observer.snapshot().cleanup_failed
    );
    actual.reverse();
    actual
}

#[cfg(target_os = "macos")]
#[test]
fn full_continuation_matches_complete_original_lb_and_shape_oracle() {
    let font = font();
    // The second fixture reproduces a short edited prefix before a long unbroken
    // word. Complete mode independently derives line/glyph results rather than
    // treating frontier candidate coordinates as their own expected answer.
    for text in [
        "hello (world) 12,345 next ".repeat(150),
        format!("one {}", "A".repeat(4099)),
    ] {
        let fixture = Fixture::new(&text);
        let actual = complete(&fixture, &font, true);
        let expected = complete(&fixture, &font, false);
        assert_eq!(actual.len(), expected.len());
        for ((line, glyphs), (oracle, expected_glyphs)) in actual.into_iter().zip(expected) {
            assert_eq!(
                (
                    line.source_utf16,
                    line.paragraph_utf16,
                    line.ordinal,
                    line.kind,
                    line.x_advance_26_6,
                    line.y_advance_26_6,
                    line.glyphs
                ),
                (
                    oracle.source_utf16,
                    oracle.paragraph_utf16,
                    oracle.ordinal,
                    oracle.kind,
                    oracle.x_advance_26_6,
                    oracle.y_advance_26_6,
                    oracle.glyphs
                )
            );
            assert_eq!(glyphs, expected_glyphs); // Preserve global clusters, offsets, advances, and unsafe flags.
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn source_eio_precedes_cancel_while_waiting_and_drops_every_actual_owner() {
    let font = font();
    let fixture = Fixture::new(&"hello world ".repeat(400));
    let cancellation = MarkdownLineComposeCancellation::new();
    let (mut worker, registered) = fixture.worker(&font, true, cancellation.clone());
    let resources = worker.worker_resource_observer();
    let backing = fixture.source.original_backing_observer().unwrap();
    let mut waiting = false;
    for _ in 0..100_000 {
        if let MarkdownLineComposeAnswer::NeedStyle(request) = worker
            .step(MarkdownLineComposeBudget::default())
            .unwrap()
            .answer
        {
            respond(&mut worker, request, &registered);
        }
        if worker.payload.as_ref().unwrap().line_breaks_pending {
            waiting = true;
            break;
        }
    }
    assert!(waiting);
    // Use the existing injection through real backing reclaim at the observed
    // pending dependency boundary. This is not a naturally induced kernel error.
    fixture.document.fail_next_original_reclaim_for_test();
    assert!(matches!(
        fixture.document.reclaim_original_read_windows_for_test(),
        Err(crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO))
    ));
    cancellation.cancel();
    assert!(matches!(
        worker.step(MarkdownLineComposeBudget::default()),
        Err(MarkdownLineComposeError::Source(
            crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
    let closed = resources.snapshot().unwrap();
    assert!(!closed.line_breaks.unwrap().metadata_file_open && !closed.writer.unwrap().file_open);
    assert_eq!(closed.shapes.active, 0);
    drop((worker, registered, fixture));
    let released = resources.snapshot().unwrap();
    assert!(released.source_payload_released);
    assert_eq!(released.first_source_errno, Some(libc::EIO));
    assert!(!resources.is_released()); // Real cleanup cannot erase the first I/O failure.
    assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0);
}

#[test]
#[ignore = "需要真实Native Body14字体输入；设置OMNI_FRONTIER_UIFONT_FIXTURES"]
fn actual_body14_natural_and_unbroken_first_line_public_work_is_equal_for_4k_8k_4m() {
    // Keep the actual UIFont name table, axes, and point size together. A same-named
    // font or monospace substitute cannot certify the real body-font path.
    let root = std::path::PathBuf::from(
        std::env::var_os("OMNI_FRONTIER_UIFONT_FIXTURES").expect("需要真实UIFont输入"),
    );
    let rows: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("fonts.json")).unwrap()).unwrap();
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["label"] == "body_system")
        .unwrap();
    assert_eq!(row["points"].as_f64().unwrap(), 14.0);
    let axes: Vec<_> = row["axes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|axis| crate::MarkdownLayoutFontAxis {
            tag: (axis["tag"].as_u64().unwrap() as u32).to_be_bytes(),
            value: axis["value"].as_f64().unwrap() as f32,
        })
        .collect();
    let font = MarkdownLayoutFont::resolve_name_table_at_size(
        std::sync::Arc::new(std::fs::read(row["path"].as_str().unwrap()).unwrap()),
        &std::fs::read(root.join(row["name_file"].as_str().unwrap())).unwrap(),
        &axes,
        14.0,
    )
    .unwrap();
    for (prefix, unit) in [("", "hello world "), ("", "A"), ("one ", "A")] {
        let mut observed = Vec::new();
        for length in [4096usize, 8192, 4 * 1024 * 1024] {
            let mut text = unit.repeat(length.div_ceil(unit.len()));
            text.truncate(length);
            // Preserve the short-word/long-word seam whose natural break lies at
            // the original glyph-part end. Changing separators/length would avoid
            // the real continuation path this fixture must exercise.
            text.insert_str(0, prefix);
            let fixture = Fixture::new(&text);
            let (mut worker, registered) =
                fixture.worker(&font, true, MarkdownLineComposeCancellation::new());
            let mut first = None;
            let mut bytes = 0;
            for _ in 0..200_000 {
                let progress = worker.step(MarkdownLineComposeBudget::default()).unwrap();
                bytes += progress.bytes;
                match progress.answer {
                    MarkdownLineComposeAnswer::Yielded => {}
                    MarkdownLineComposeAnswer::NeedStyle(request) => {
                        let range = request.source_range();
                        worker
                            .provide_style(
                                request
                                    .respond_registered(
                                        range,
                                        &registered,
                                        MarkdownLineComposeStyle {
                                            presentation: Default::default(),
                                            points: 14.0,
                                            language: None,
                                            features: Vec::new(),
                                        },
                                    )
                                    .unwrap(),
                            )
                            .unwrap();
                    }
                    MarkdownLineComposeAnswer::LineReady(line) => {
                        first = Some(line);
                        break;
                    }
                    _ => panic!("实际Body14已认证字体必须保留原自然/紧急选行"),
                }
            }
            let first = first.expect("实际Body14必须签真实LineEnd");
            if !prefix.is_empty() {
                assert_eq!(first.source_utf16, 0..prefix.len());
                assert_eq!(first.kind, MarkdownComposedLineKind::Allowed);
            }
            // Public bounded work and actual first-line geometry must remain
            // independent of the unconsumed long suffix's length.
            observed.push((
                bytes,
                first.source_utf16.end,
                first.x_advance_26_6,
                worker.frontier_work().unwrap(),
            ));
            worker.into_output().unwrap().close().unwrap();
        }
        assert!(observed.windows(2).all(|pair| pair[0] == pair[1]));
        eprintln!(
            "actual_body14_lazy_lb prefix={prefix:?} unit={unit:?} work={observed:?} whole_product_pass=false"
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn cancel_before_the_deferred_lb_turn_releases_real_resources() {
    let font = font();
    let fixture = Fixture::new(&"hello world ".repeat(400));
    let cancellation = MarkdownLineComposeCancellation::new();
    let (mut worker, registered) = fixture.worker(&font, true, cancellation.clone());
    let resources = worker.worker_resource_observer();
    let backing = fixture.source.original_backing_observer().unwrap();
    let mut waiting = false;
    for _ in 0..100_000 {
        if let MarkdownLineComposeAnswer::NeedStyle(request) = worker
            .step(MarkdownLineComposeBudget::default())
            .unwrap()
            .answer
        {
            respond(&mut worker, request, &registered);
        }
        if worker.payload.as_ref().unwrap().line_breaks_pending {
            waiting = true;
            break;
        }
    }
    assert!(waiting);
    cancellation.cancel();
    assert!(matches!(
        worker.step(MarkdownLineComposeBudget::default()),
        Err(MarkdownLineComposeError::Cancelled)
    ));
    let closed = resources.snapshot().unwrap();
    assert!(!closed.line_breaks.unwrap().metadata_file_open);
    assert_eq!(closed.line_breaks.unwrap().metadata_peak_map_bytes, 0); // No actual opportunity page was mapped yet.
    assert!(!closed.writer.unwrap().file_open && closed.shapes.active == 0);
    drop((worker, registered, fixture));
    assert!(resources.is_released());
    assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
}
