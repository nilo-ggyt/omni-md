//! Finite complete oracles check actual glyph fields. These tests do not establish Native layout
//! acceptance or whole-product peak-memory acceptance.
#[path = "advance_tests.rs"]
mod advance_tests;
#[path = "local_tests.rs"]
mod local_tests;
#[path = "positioned_tests.rs"]
mod positioned_tests;
#[path = "terminal_tests.rs"]
mod terminal_tests;
#[path = "unicode_tests.rs"]
mod unicode_tests;
use super::*;
use crate::{
    MarkdownDocumentBuffer, MarkdownGlyphClusterIndex, MarkdownGlyphClusterStatus,
    MarkdownLineBreakBudget, MarkdownSoftLineMeasureAnswer, MarkdownSoftLineMeasureBudget,
    MarkdownSoftLineMeasureCancellation, MarkdownSoftLineMeasureChoice, MarkdownSoftLineMeasureJob,
    MarkdownSoftLineMeasureRequest,
};
use std::collections::BTreeMap;

const ROBOTO: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../vendor/harfrust/benches/fonts/Roboto-Regular.ttf"
));

fn read16(bytes: &[u8], at: usize) -> usize {
    usize::from(u16::from_be_bytes(bytes[at..at + 2].try_into().unwrap()))
}
fn read32(bytes: &[u8], at: usize) -> usize {
    u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize
}
fn independent_font() -> MarkdownLayoutFont {
    // This finite test font omits cross-input tables while retaining actual cmap/outlines/metrics/
    // names. Names grant no product capability: production inspects the real plan, and unmodified
    // Roboto remains an explicit negative control below.
    let mut tables = BTreeMap::new();
    for row in 0..read16(ROBOTO, 4) {
        let row = 12 + row * 16;
        let tag: [u8; 4] = ROBOTO[row..row + 4].try_into().unwrap();
        if [*b"GSUB", *b"GPOS", *b"kern", *b"trak", *b"morx", *b"kerx"].contains(&tag) {
            continue;
        }
        let at = read32(ROBOTO, row + 8);
        tables.insert(tag, &ROBOTO[at..at + read32(ROBOTO, row + 12)]);
    }
    let mut result = vec![0; 12 + tables.len() * 16];
    result[..4].copy_from_slice(&[0, 1, 0, 0]);
    result[4..6].copy_from_slice(&(tables.len() as u16).to_be_bytes());
    for (row, (tag, table)) in tables.into_iter().enumerate() {
        let offset = result.len();
        let row = 12 + row * 16;
        result[row..row + 4].copy_from_slice(&tag);
        result[row + 8..row + 12].copy_from_slice(&(offset as u32).to_be_bytes());
        result[row + 12..row + 16].copy_from_slice(&(table.len() as u32).to_be_bytes());
        result.extend_from_slice(table);
        while !result.len().is_multiple_of(4) {
            result.push(0);
        }
    }
    MarkdownLayoutFont::resolve(Arc::new(result), "Roboto-Regular", &[]).unwrap()
}

struct Fixture {
    _directory: tempfile::TempDir,
    document: MarkdownDocumentBuffer,
    source: MarkdownLayoutTextSource,
    authority: MarkdownLayoutTextAuthority,
    font: MarkdownLayoutFont,
    font_authority: MarkdownLayoutFontAuthority,
    breaks: MarkdownLayoutBreakJob,
}
impl Fixture {
    fn new(text: &str, font: MarkdownLayoutFont) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("前沿合成控制.md");
        std::fs::write(&path, text).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, text.len() as u64 + 65536).unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let authority = source.authority();
        let font_authority = font.authority();
        let mut breaks = MarkdownLayoutBreakJob::new(source.clone(), &authority).unwrap();
        // Advance only a fixed prefix; complete-item break preprocessing is not free first-line work.
        for _ in 0..4 {
            breaks
                .step(
                    &authority,
                    MarkdownLineBreakBudget {
                        bytes: 1024,
                        scalars: 1024,
                        records: 1024,
                    },
                )
                .unwrap();
        }
        Self {
            _directory: directory,
            document,
            source,
            authority,
            font,
            font_authority,
            breaks,
        }
    }
    fn options(&self) -> MarkdownShapingOptions {
        MarkdownShapingOptions {
            direction: MarkdownShapingDirection::LeftToRight,
            script: *b"Latn",
            language: Some("en".into()),
            points: self.font.shaping_point_size().unwrap_or(16.0) as f32,
            beginning_of_text: true,
            end_of_text: true,
            features: Vec::new(),
        }
    }
    fn frontier(&self, range: Range<usize>) -> MarkdownShapingFrontierJob {
        MarkdownShapingFrontierJob::new(
            self.source.clone(),
            &self.authority,
            self.font.clone(),
            &self.font_authority,
            range,
            self.options(),
            MarkdownShapingCancellation::default(),
        )
        .unwrap()
    }
    fn first(
        &mut self,
        frontier: &mut MarkdownShapingFrontierJob,
    ) -> (MarkdownShapingFrontierPart, usize, usize) {
        // Yield each actual input quantum and retain the unresolved answer until a real part can
        // be taken. The finite loop limit is failure detection, not a fabricated EOF or Ready.
        let mut input = 0;
        let mut reads = 0;
        for _ in 0..4096 {
            let step = frontier
                .step(
                    &self.authority,
                    &self.font_authority,
                    &mut self.breaks,
                    MarkdownShapingBudget {
                        bytes: 31,
                        scalars: 31,
                    },
                    128,
                )
                .unwrap();
            assert!(step.source_bytes + step.input_bytes <= 31 && step.shapes <= 1);
            reads += step.source_bytes;
            input += step.input_bytes;
            match step.answer {
                MarkdownShapingFrontierAnswer::PartReady => {
                    return (
                        frontier
                            .take_part(&self.authority, &self.font_authority)
                            .unwrap(),
                        input,
                        reads,
                    );
                }
                MarkdownShapingFrontierAnswer::Pending => {}
                other => panic!("前沿未完成: {other:?}"),
            }
        }
        panic!("有限样本不应挂起")
    }
    fn oracle(&self, range: Range<usize>) -> MarkdownShapingJob {
        // Independently shape the finite complete range on the same actual Source/font/options,
        // without consuming frontier certificates or center-view output as the expected result.
        let mut job = MarkdownShapingJob::new(
            self.source.clone(),
            &self.authority,
            self.font.clone(),
            &self.font_authority,
            range,
            self.options(),
        )
        .unwrap();
        while job.status() == MarkdownShapingStatus::Ingesting {
            job.step(
                &self.authority,
                &self.font_authority,
                MarkdownShapingBudget::default(),
            )
            .unwrap();
        }
        job.shape(&self.authority, &self.font_authority).unwrap();
        job
    }
}

#[test]
fn certified_first_natural_line_work_is_constant_for_4k_8k_32k_original_items() {
    let mut work = Vec::new(); // Three fixed control observations, not a length-dependent production cache.
    for units in [4096, 8192, 32768] {
        let text = "hello world ".repeat(units / 12 + 1);
        let mut fixture = Fixture::new(&text, independent_font());
        let end = fixture.source.utf16_len();
        let mut frontier = fixture.frontier(0..end);
        let (part, input, reads) = fixture.first(&mut frontier);
        let range = part.certified_range();
        assert_eq!(part.original_range(), 0..end);
        assert!(!part.is_item_complete() && range.len() <= 256);
        let (mut child, ticket) = part.into_parts();
        assert!(
            child
                .payload
                .as_ref()
                .unwrap()
                .buffer
                .flags()
                .contains(BufferFlags::BEGINNING_OF_TEXT)
        );
        assert!(
            !child
                .payload
                .as_ref()
                .unwrap()
                .buffer
                .flags()
                .contains(BufferFlags::END_OF_TEXT)
        );
        let mut oracle = fixture.oracle(0..end);
        let mut expected = 0;
        for ordinal in 0..oracle.profile().unwrap().glyph_count {
            let glyph = oracle
                .glyph(&fixture.authority, &fixture.font_authority, ordinal)
                .unwrap()
                .unwrap();
            if range.contains(&glyph.source_utf16_cluster) {
                assert_eq!(
                    child
                        .glyph(&fixture.authority, &fixture.font_authority, expected)
                        .unwrap(),
                    Some(glyph)
                );
                expected += 1;
            }
        }
        assert_eq!(expected, child.profile().unwrap().glyph_count);
        let mut clusters =
            MarkdownGlyphClusterIndex::new(&mut child, &fixture.authority, &fixture.font_authority)
                .unwrap();
        while clusters.status() != MarkdownGlyphClusterStatus::Ready {
            clusters.step(&mut child, 17).unwrap();
        }
        let mut measure = MarkdownSoftLineMeasureJob::new(
            fixture.source.clone(),
            &fixture.authority,
            &fixture.font_authority,
            &mut child,
            &mut fixture.breaks,
            MarkdownSoftLineMeasureRequest {
                start_utf16: 0,
                width_26_6: 5000,
                cancellation: MarkdownSoftLineMeasureCancellation::default(),
            },
        )
        .unwrap();
        let mut measured = None;
        // PartReady alone is insufficient: complete actual cluster indexing and soft-line measure,
        // then compare the selected line across original item lengths.
        for _ in 0..1024 {
            match measure
                .step(
                    &mut child,
                    &mut clusters,
                    &mut fixture.breaks,
                    MarkdownSoftLineMeasureBudget::default(),
                )
                .unwrap()
                .answer
            {
                MarkdownSoftLineMeasureAnswer::Selected(
                    MarkdownSoftLineMeasureChoice::MeasuredBreak(value),
                ) => {
                    measured = Some(value);
                    break;
                }
                MarkdownSoftLineMeasureAnswer::Yielded => {}
                other => panic!("尚非真实首个安全软行: {other:?}"),
            }
        }
        let measured = measured.unwrap();
        assert!(measured.source_utf16.end < range.end);
        work.push((input, reads, measured.source_utf16.end));
        assert_eq!(
            frontier
                .step(
                    &fixture.authority,
                    &fixture.font_authority,
                    &mut fixture.breaks,
                    MarkdownShapingBudget::default(),
                    128,
                )
                .unwrap()
                .answer,
            MarkdownShapingFrontierAnswer::PendingConsumer
        );
        let identity = Arc::downgrade(&child.identity);
        drop(clusters);
        drop(measure);
        drop(child);
        assert_eq!(identity.strong_count(), 0);
        frontier
            .resume(&fixture.authority, &fixture.font_authority, ticket)
            .unwrap();
        let (next, _, _) = fixture.first(&mut frontier);
        assert_eq!(next.certified_range().start, range.end);
    }
    assert!(work.windows(2).all(|pair| pair[0] == pair[1]));
    eprintln!("frontier_actual_first_line work={work:?}");
}

#[test]
fn source_font_cancel_and_io_revocation_are_fail_closed_with_real_release() {
    // Exercise distinct original-authority failures. Actual Source reclaim EIO must retain priority
    // over later cancellation, and final cleanup must release the real captured payload/mapping.
    let mut fixture = Fixture::new(&"word ".repeat(256), independent_font());
    let observer = fixture.source.resource_observer();
    let backing = fixture.document.original_backing_observer().unwrap();
    let mut frontier = fixture.frontier(0..fixture.source.utf16_len());
    let other = MarkdownLayoutTextSource::capture(&fixture.document);
    assert!(matches!(
        frontier.step(
            &other.authority(),
            &fixture.font_authority,
            &mut fixture.breaks,
            MarkdownShapingBudget::default(),
            128,
        ),
        Err(MarkdownShapingFrontierError::Shaping(
            MarkdownShapingError::SourceMismatch
        ))
    ));
    let mut frontier = fixture.frontier(0..fixture.source.utf16_len());
    let another_font = independent_font();
    assert!(matches!(
        frontier.step(
            &fixture.authority,
            &another_font.authority(),
            &mut fixture.breaks,
            MarkdownShapingBudget::default(),
            128,
        ),
        Err(MarkdownShapingFrontierError::Shaping(
            MarkdownShapingError::FontMismatch
        ))
    ));
    let mut frontier = fixture.frontier(0..fixture.source.utf16_len());
    frontier.cancellation.request();
    assert!(matches!(
        frontier.step(
            &fixture.authority,
            &fixture.font_authority,
            &mut fixture.breaks,
            MarkdownShapingBudget::default(),
            128,
        ),
        Err(MarkdownShapingFrontierError::Shaping(
            MarkdownShapingError::Cancelled
        ))
    ));
    let mut frontier = fixture.frontier(0..fixture.source.utf16_len());
    fixture.source.visit_byte_range(0..1, |_| Ok(())).unwrap();
    fixture.document.fail_next_original_reclaim_for_test();
    assert!(
        fixture
            .document
            .reclaim_original_read_windows_for_test()
            .is_err()
    );
    frontier.cancellation.request();
    assert!(matches!(
        frontier.step(
            &fixture.authority,
            &fixture.font_authority,
            &mut fixture.breaks,
            MarkdownShapingBudget::default(),
            128,
        ),
        Err(MarkdownShapingFrontierError::Shaping(
            MarkdownShapingError::Source(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                libc::EIO
            ))
        ))
    ));
    drop(frontier);
    drop(fixture);
    drop(other);
    assert!(observer.payload_released() && backing.unmap_succeeded());
}

#[test]
fn unsupported_plan_missing_glyph_and_uncertified_word_do_not_acquire_glyph_ready() {
    let actual = MarkdownLayoutFont::resolve(Arc::new(ROBOTO), "Roboto-Regular", &[]).unwrap();
    let mut fixture = Fixture::new("hello world ", actual);
    let mut frontier = fixture.frontier(0..fixture.source.utf16_len());
    let mut rejected = false;
    for _ in 0..8192 {
        let answer = frontier
            .step(
                &fixture.authority,
                &fixture.font_authority,
                &mut fixture.breaks,
                MarkdownShapingBudget::default(),
                128,
            )
            .unwrap()
            .answer;
        if matches!(
            answer,
            MarkdownShapingFrontierAnswer::NeedsFrontier(MarkdownShapingFrontierReason::Plan(_))
        ) {
            rejected = true;
            break;
        }
        assert_eq!(answer, MarkdownShapingFrontierAnswer::Pending);
        assert!(frontier.child.is_none());
    } // Actual reachable Roboto ligatures remain rejected; per-record certification may yield rather than scan the font during construction.
    assert!(rejected);
    assert!(frontier.child.is_none());
    for (text, reason) in [
        (
            format!("{}\0", "A".repeat(8192)),
            MarkdownShapingFrontierReason::UncertifiedWord,
        ),
        (
            "中文".repeat(128),
            MarkdownShapingFrontierReason::Plan(harfrust::AsciiFrontierRejection::MissingGlyph),
        ),
    ] {
        let mut fixture = Fixture::new(&text, independent_font());
        let mut frontier = fixture.frontier(0..fixture.source.utf16_len());
        let step = frontier
            .step(
                &fixture.authority,
                &fixture.font_authority,
                &mut fixture.breaks,
                MarkdownShapingBudget::default(),
                128,
            )
            .unwrap();
        assert_eq!(
            step.answer,
            MarkdownShapingFrontierAnswer::NeedsFrontier(reason)
        );
        assert_eq!((step.input_bytes, step.shapes), (0, 0));
        assert!(frontier.child.is_none());
    }
}

#[test]
fn continuation_requires_original_owner_actual_child_drop_and_global_nonzero_source() {
    let text = format!("前🙂\n{}", "word ".repeat(128));
    let mut fixture = Fixture::new(&text, independent_font());
    let mut frontier = fixture.frontier(4..fixture.source.utf16_len());
    let (part, _, _) = fixture.first(&mut frontier);
    let range = part.certified_range();
    let (mut child, ticket) = part.into_parts();
    assert_eq!(
        child
            .glyph(&fixture.authority, &fixture.font_authority, 0)
            .unwrap()
            .unwrap()
            .source_utf16_cluster,
        4
    );
    assert!(
        !child
            .payload
            .as_ref()
            .unwrap()
            .buffer
            .flags()
            .contains(BufferFlags::END_OF_TEXT)
    );
    let mut foreign = fixture.frontier(4..fixture.source.utf16_len());
    assert!(matches!(
        foreign.resume(&fixture.authority, &fixture.font_authority, ticket),
        Err(MarkdownShapingFrontierError::ContinuationMismatch)
    ));
    assert!(foreign.payload.is_none());
    // Consuming the wrong ticket on a foreign owner does not advance the original. Its actual
    // child remains alive; no replacement ticket can silently bypass consumption.
    assert_eq!(
        frontier
            .step(
                &fixture.authority,
                &fixture.font_authority,
                &mut fixture.breaks,
                MarkdownShapingBudget::default(),
                128,
            )
            .unwrap()
            .answer,
        MarkdownShapingFrontierAnswer::PendingConsumer
    );
    assert_eq!(frontier.next, range.start);
    drop(child);
}

#[test]
fn actual_gcb_unknown_mismatch_and_true_item_eof_are_not_chunk_eof() {
    // ASCII after a mark still needs actual GCB proof; an item ending between base and Extend has
    // no permission to make that interior position a shaping center boundary.
    let mut fixture = Fixture::new("a\u{301}word ", independent_font());
    let mut frontier = fixture.frontier(0..1);
    let step = frontier
        .step(
            &fixture.authority,
            &fixture.font_authority,
            &mut fixture.breaks,
            MarkdownShapingBudget::default(),
            128,
        )
        .unwrap();
    assert_eq!(
        step.answer,
        MarkdownShapingFrontierAnswer::NeedsFrontier(
            MarkdownShapingFrontierReason::NotGraphemeBoundary
        )
    );
    assert!(frontier.child.is_none());
    let mut fixture = Fixture::new("word ", independent_font());
    let mut frontier = fixture.frontier(0..5);
    let (part, _, _) = fixture.first(&mut frontier);
    assert!(part.is_item_complete());
    let (child, continuation) = part.into_parts();
    assert!(
        child
            .payload
            .as_ref()
            .unwrap()
            .buffer
            .flags()
            .contains(BufferFlags::END_OF_TEXT)
    );
    let resources = child.resource_observer();
    drop(child);
    assert_eq!(resources.snapshot().files_open, 0);
    frontier
        .resume(&fixture.authority, &fixture.font_authority, continuation)
        .unwrap();
    assert_eq!(
        frontier
            .step(
                &fixture.authority,
                &fixture.font_authority,
                &mut fixture.breaks,
                MarkdownShapingBudget::default(),
                128,
            )
            .unwrap()
            .answer,
        MarkdownShapingFrontierAnswer::Complete
    );
    let mut fixture = Fixture::new("😀 word ", independent_font());
    assert!(matches!(
        MarkdownShapingFrontierJob::new(
            fixture.source.clone(),
            &fixture.authority,
            fixture.font.clone(),
            &fixture.font_authority,
            1..fixture.source.utf16_len(),
            fixture.options(),
            MarkdownShapingCancellation::default()
        ),
        Err(MarkdownShapingFrontierError::Shaping(
            MarkdownShapingError::Source(_)
        ))
    ));
    // The original BreakJob remains required; a new unknown index cannot become Ready merely
    // because common ASCII rules usually allow a boundary.
    fixture.breaks =
        MarkdownLayoutBreakJob::new(fixture.source.clone(), &fixture.authority).unwrap();
    let mut frontier = fixture.frontier(3..8);
    let step = frontier
        .step(
            &fixture.authority,
            &fixture.font_authority,
            &mut fixture.breaks,
            MarkdownShapingBudget::default(),
            128,
        )
        .unwrap();
    assert_eq!(step.answer, MarkdownShapingFrontierAnswer::Pending);
    assert!(frontier.child.is_none());
}

#[cfg(target_os = "macos")]
#[test]
fn real_system_monospace_active_plan_matches_full_finite_glyph_oracle() {
    // Use this machine's actual system font as a positive control only. Product capability still
    // checks the plan record by record; neither this path nor its name authorizes a shortcut.
    let bytes: Arc<dyn AsRef<[u8]> + Send + Sync> =
        Arc::new(std::fs::read("/System/Library/Fonts/SFNSMono.ttf").unwrap());
    let font = MarkdownLayoutFont::resolve(bytes, ".SFNSMono-Regular", &[]).unwrap();
    let text = "the actual ordered plan is checked 0123456789 ".repeat(100);
    let mut fixture = Fixture::new(&text, font);
    let mut frontier = fixture.frontier(0..fixture.source.utf16_len());
    let (part, input, _) = fixture.first(&mut frontier);
    let range = part.certified_range();
    let (mut child, continuation) = part.into_parts();
    let mut full = fixture.oracle(0..fixture.source.utf16_len());
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
                Some(glyph)
            );
            expected += 1;
        }
    }
    assert_eq!(child.profile().unwrap().glyph_count, expected);
    assert_eq!(input, range.len());
    eprintln!(
        "frontier_system_mono actual_lookups={:?} input={input} glyphs={expected}",
        frontier
            .payload
            .as_ref()
            .unwrap()
            .plan
            .as_ref()
            .unwrap()
            .capability()
    );
    drop(child);
    frontier
        .resume(&fixture.authority, &fixture.font_authority, continuation)
        .unwrap();
}

#[test]
fn certified_unbroken_ascii_first_emergency_uses_true_gcb_without_word_eof_ingest() {
    use crate::markdown_soft_line_measure::provider::{JobGlyphProvider, SelectedMeasureKind};
    let mut observed = Vec::new(); // Three finite observations, without a product vector growing per line or word.
    for size in [4096, 8192, 32768] {
        let mut fixture = Fixture::new(&"A".repeat(size), independent_font());
        let mut frontier = fixture.frontier(0..size);
        let mut glyph_metadata = 0;
        let mut plan_metadata = 0;
        let mut input = 0;
        for _ in 0..65536 {
            let step = frontier
                .step(
                    &fixture.authority,
                    &fixture.font_authority,
                    &mut fixture.breaks,
                    MarkdownShapingBudget {
                        bytes: 17,
                        scalars: 17,
                    },
                    1,
                )
                .unwrap();
            assert!(step.plan_records + step.metadata_glyphs <= 1);
            input += step.input_bytes;
            glyph_metadata += step.metadata_glyphs;
            plan_metadata += step.plan_records;
            if step.answer == MarkdownShapingFrontierAnswer::PartReady {
                break;
            }
            assert_eq!(step.answer, MarkdownShapingFrontierAnswer::Pending);
        }
        let part = frontier
            .take_part(&fixture.authority, &fixture.font_authority)
            .unwrap();
        assert_eq!(part.certified_range(), 0..256);
        let (mut child, ticket) = part.into_parts();
        assert_eq!(
            child
                .frontier_word_end(&fixture.authority, &fixture.font_authority)
                .unwrap(),
            Some(size)
        );
        let mut clusters =
            MarkdownGlyphClusterIndex::new(&mut child, &fixture.authority, &fixture.font_authority)
                .unwrap();
        while clusters.status() != MarkdownGlyphClusterStatus::Ready {
            clusters.step(&mut child, 7).unwrap();
        }
        let mut measure = MarkdownSoftLineMeasureJob::new(
            fixture.source.clone(),
            &fixture.authority,
            &fixture.font_authority,
            &mut child,
            &mut fixture.breaks,
            MarkdownSoftLineMeasureRequest {
                start_utf16: 0,
                width_26_6: 5000,
                cancellation: MarkdownSoftLineMeasureCancellation::default(),
            },
        )
        .unwrap();
        let mut choice = None;
        for _ in 0..2048 {
            let step = measure
                .step(
                    &mut child,
                    &mut clusters,
                    &mut fixture.breaks,
                    MarkdownSoftLineMeasureBudget {
                        break_queries: 1,
                        clusters: 1,
                    },
                )
                .unwrap();
            assert!(step.break_queries <= 1 && step.clusters <= 1);
            if let MarkdownSoftLineMeasureAnswer::Selected(value) = step.answer {
                choice = Some(value);
                break;
            }
        }
        assert!(matches!(
            choice,
            Some(MarkdownSoftLineMeasureChoice::MeasuredEmergencyBreak { ref source_utf16, .. })
                if source_utf16.start == 0 && source_utf16.end < 256
        ));
        let selected = {
            let mut provider = JobGlyphProvider::new(&mut child, Some(&mut clusters));
            let permit = measure.selected_permit(&mut provider).unwrap();
            permit.require(&mut provider).unwrap().selected
        }; // Store consumes the same confined permit; a public Choice copy cannot sign that authority.
        assert_eq!(selected.source_utf16.start, 0);
        assert!(selected.source_utf16.end < 256 && selected.x_advance_26_6 <= 5000);
        assert_eq!(
            selected.boundary_flags & harfrust::GlyphFlags::UNSAFE_TO_BREAK.to_bits(),
            0
        );
        assert_eq!(
            selected.kind,
            SelectedMeasureKind::Emergency {
                overflowing_grapheme: false
            }
        );
        let mut full = fixture.oracle(0..size);
        for ordinal in 0..child.profile().unwrap().glyph_count {
            let actual = child
                .glyph(&fixture.authority, &fixture.font_authority, ordinal)
                .unwrap()
                .unwrap();
            if selected.source_utf16.contains(&actual.source_utf16_cluster) {
                assert_eq!(
                    Some(actual),
                    full.glyph(&fixture.authority, &fixture.font_authority, ordinal)
                        .unwrap()
                );
            }
        }
        observed.push((
            input,
            glyph_metadata,
            plan_metadata,
            selected.source_utf16.end,
        ));
        drop(measure);
        drop(clusters);
        drop(child);
        frontier
            .resume(&fixture.authority, &fixture.font_authority, ticket)
            .unwrap();
    }
    assert!(observed.windows(2).all(|pair| pair[0] == pair[1]));
    eprintln!("frontier_unbroken_actual work={observed:?}");
}
