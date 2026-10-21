//! Compare finite synthetic inputs against complete HarfRust oracles and actual Core Source
//! resource observations. These controls instantiate no Native graph or replacement product path.
use super::*;
use crate::{
    MarkdownDocumentBuffer, MarkdownGlyphClusterStatus, MarkdownLayoutFont,
    MarkdownLayoutFontAuthority, MarkdownLayoutTextAuthority, MarkdownLayoutTextSource,
    MarkdownLineBreakBudget, MarkdownShapingDirection, MarkdownShapingFeature,
    MarkdownShapingOptions, MarkdownSoftLineMeasureAnswer, MarkdownSoftLineMeasureBudget,
    MarkdownSoftLineMeasureRequest,
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
    font: MarkdownLayoutFont,
    font_authority: MarkdownLayoutFontAuthority,
    options: MarkdownShapingOptions,
    shape: MarkdownShapingJob,
    clusters: MarkdownGlyphClusterIndex,
    breaks: MarkdownLayoutBreakJob,
}
impl Fixture {
    fn new(text: &str, item: Range<usize>, direction: MarkdownShapingDirection) -> Self {
        // Own the finite file, actual frozen Source, resolved font, and original shape locally.
        // Whole fixture/oracle text exists only in this test, not the production Source interface.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("实际软行子范围.md");
        std::fs::write(&path, text).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, text.len() as u64 + 65536).unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let authority = source.authority();
        let font = MarkdownLayoutFont::resolve(Arc::new(ROBOTO), "Roboto-Regular", &[]).unwrap();
        let font_authority = font.authority();
        let options = MarkdownShapingOptions {
            direction,
            script: *b"Latn",
            language: Some("en".into()),
            points: 16.0,
            beginning_of_text: item.start == 0
                || text.encode_utf16().nth(item.start - 1) == Some(10),
            end_of_text: item.end == source.utf16_len(),
            features: vec![MarkdownShapingFeature {
                tag: *b"liga",
                value: 1,
                source_utf16_start: item.start as _,
                source_utf16_end: item.end as _,
            }],
        };
        // Preserve original global feature coordinates and actual paragraph-edge flags. A later
        // soft boundary must not manufacture new BOT/EOT or discard adjacent context.
        let mut shape = MarkdownShapingJob::new(
            source.clone(),
            &authority,
            font.clone(),
            &font_authority,
            item,
            options.clone(),
        )
        .unwrap();
        fill(&mut shape, &authority, &font_authority);
        // Complete actual cluster and GCB/UAX indexes through their bounded production steps
        // before the cases below ask the resolver to distinguish a known answer from Pending.
        let mut clusters =
            MarkdownGlyphClusterIndex::new(&mut shape, &authority, &font_authority).unwrap();
        while clusters.status() != MarkdownGlyphClusterStatus::Ready {
            clusters.step(&mut shape, 11).unwrap();
        }
        let mut breaks = MarkdownLayoutBreakJob::new(source.clone(), &authority).unwrap();
        while !breaks
            .step(&authority, MarkdownLineBreakBudget::default())
            .unwrap()
            .ready
        {}
        Self {
            _directory: directory,
            document,
            source,
            authority,
            font,
            font_authority,
            options,
            shape,
            clusters,
            breaks,
        }
    }
    fn measure(&mut self, start: usize, width: i64) -> MarkdownSoftLineMeasureJob {
        // Each call creates a real original measure with its own nonce and cancellation, even
        // when Source, start, and width match another measure numerically.
        let mut job = MarkdownSoftLineMeasureJob::new(
            self.source.clone(),
            &self.authority,
            &self.font_authority,
            &mut self.shape,
            &mut self.breaks,
            MarkdownSoftLineMeasureRequest {
                start_utf16: start,
                width_26_6: width,
                cancellation: MarkdownSoftLineMeasureCancellation::new(),
            },
        )
        .unwrap();
        loop {
            match job
                .step(
                    &mut self.shape,
                    &mut self.clusters,
                    &mut self.breaks,
                    MarkdownSoftLineMeasureBudget::default(),
                )
                .unwrap()
                .answer
            {
                MarkdownSoftLineMeasureAnswer::Selected(_) => break,
                MarkdownSoftLineMeasureAnswer::Yielded => {}
                MarkdownSoftLineMeasureAnswer::PendingDependencies => panic!("真实依赖已完成"),
            }
        }
        job
    }
    fn resolve(
        &mut self,
        measure: &MarkdownSoftLineMeasureJob,
        budget: MarkdownSoftLineResolveBudget,
    ) -> (MarkdownSoftLineResolveJob, MarkdownSoftLineResolveAnswer) {
        // Drive only actual bounded resolver turns. A finite test iteration guard fails loudly;
        // it cannot convert unfinished dependencies or timeout into a successful resolved boundary.
        let mut job = MarkdownSoftLineResolveJob::new(measure, &mut self.shape).unwrap();
        for _ in 0..2_000_000 {
            let step = job
                .step(
                    measure,
                    &mut self.shape,
                    &mut self.clusters,
                    &mut self.breaks,
                    budget,
                )
                .unwrap();
            assert!(step.input_bytes <= budget.input_bytes.min(4096));
            assert!(step.glyphs <= budget.glyphs.min(MARKDOWN_SOFT_LINE_RESOLVE_GLYPHS));
            assert!(
                step.candidates <= budget.candidates.min(MARKDOWN_SOFT_LINE_RESOLVE_CANDIDATES)
            );
            assert!(step.shapes <= 1);
            // Work counts check this Core turn's contract, not native paint latency or kernel RSS.
            match step.answer {
                MarkdownSoftLineResolveAnswer::Yielded => {}
                MarkdownSoftLineResolveAnswer::PendingDependencies => {
                    panic!("真实GCB/cluster索引已完成")
                }
                answer => return (job, answer),
            }
        }
        panic!("有限输入未完成，不把超时标为Pending成功");
    }
}
fn fill(
    shape: &mut MarkdownShapingJob,
    source: &MarkdownLayoutTextAuthority,
    font: &MarkdownLayoutFontAuthority,
) {
    // Ingest the finite item's actual Source in small original-authority turns, then shape it
    // synchronously under this owner. The fixture starts no alternative execution framework.
    while shape.status() == MarkdownShapingStatus::Ingesting {
        shape
            .step(
                source,
                font,
                MarkdownShapingBudget {
                    bytes: 31,
                    scalars: 7,
                },
            )
            .unwrap();
    }
    shape.shape(source, font).unwrap();
}
fn tiny() -> MarkdownSoftLineResolveBudget {
    // Exercise continuation with the minimum nonzero budgets rather than bypassing bounded work.
    MarkdownSoftLineResolveBudget {
        input_bytes: 1,
        input_scalars: 1,
        glyphs: 1,
        candidates: 1,
    }
}
fn byte_offset(text: &str, utf16: usize) -> usize {
    // Map independently held finite oracle text from UTF-16 to UTF-8. All queried offsets must
    // be actual scalar boundaries; an unmatched coordinate is a test error, not a clamped slice.
    let mut units = 0;
    for (byte, scalar) in text.char_indices() {
        if units == utf16 {
            return byte;
        }
        units += scalar.len_utf16();
    }
    assert_eq!(units, utf16);
    text.len()
}

/// Independently shape one complete finite substring through public HarfRust, with original
/// global context/features rather than Core's subrange-derivation helper.
fn oracle(fixture: &Fixture, text: &str, range: Range<usize>) -> Vec<MarkdownShapedGlyph> {
    let mut buffer = harfrust::Buffer::new();
    let direction = match fixture.options.direction {
        MarkdownShapingDirection::LeftToRight => harfrust::Direction::LeftToRight,
        MarkdownShapingDirection::RightToLeft => harfrust::Direction::RightToLeft,
        _ => panic!("本控制只验证水平run"),
    };
    buffer.set_direction(direction);
    buffer.set_script(harfrust::Script::from_iso15924_tag(
        harfrust::Tag::from_be_bytes(fixture.options.script),
    ));
    buffer.set_language(harfrust::Language::new(
        fixture.options.language.as_ref().unwrap().as_bytes(),
    ));
    buffer.set_cluster_level(harfrust::ClusterLevel::MonotoneGraphemes);
    let mut flags = harfrust::BufferFlags::PRODUCE_UNSAFE_TO_CONCAT;
    let item = fixture.shape.source_range();
    // Keep original paragraph flags only at the real item endpoints. Soft-line endpoints do
    // not create artificial paragraph boundaries for this independent shaping comparison.
    if fixture.options.beginning_of_text && range.start == item.start {
        flags |= harfrust::BufferFlags::BEGINNING_OF_TEXT;
    }
    if fixture.options.end_of_text && range.end == item.end {
        flags |= harfrust::BufferFlags::END_OF_TEXT;
    }
    buffer.set_flags(flags);
    let bytes = byte_offset(text, range.start)..byte_offset(text, range.end);
    buffer.set_pre_context(&text[..bytes.start]);
    buffer.set_post_context(&text[bytes.end..]);
    // Global UTF-16 cluster coordinates and original feature intervals must remain unchanged
    // even when the independently shaped prefix begins after the document start.
    let mut units = range.start;
    for scalar in text[bytes].chars() {
        buffer.push(u32::from(scalar), units as _);
        units += scalar.len_utf16();
    }
    let features: Vec<_> = fixture
        .options
        .features
        .iter()
        .map(|feature| harfrust::Feature {
            tag: harfrust::Tag::from_be_bytes(feature.tag),
            value: feature.value,
            start: feature.source_utf16_start,
            end: feature.source_utf16_end,
        })
        .collect();
    harfrust::shape(
        &harfrust::ShaperFont::new(fixture.font.font()).with_scale(1024),
        &mut buffer,
        harfrust::ShapeOptions::new()
            .point_size(Some(16.0))
            .features(&features),
    )
    .unwrap();
    (0..buffer.len())
        .map(|index| {
            let (info, position) = buffer.paged_glyph_record(index).unwrap();
            MarkdownShapedGlyph {
                glyph_id: info.glyph_id,
                source_utf16_cluster: info.cluster as _,
                flags: info.flags().to_bits(),
                x_advance: position.x_advance,
                y_advance: position.y_advance,
                x_offset: position.x_offset,
                y_offset: position.y_offset,
            }
        })
        .collect() // Complete glyph Vec is limited to this finite oracle; production exposes no equivalent whole-text borrow/output.
}

fn transferred_line_matches_spool_oracle(
    fixture: &Fixture,
    text: &str,
    boundary: &MarkdownSoftLineResolvedBoundary,
    mut line: MarkdownSoftLineResolvedShape,
) {
    // Compare the handed-off actual child before copying its glyphs through the real spool
    // builder. The independent expected array comes from HarfRust, not the packet's facts.
    let expected = oracle(fixture, text, boundary.source_utf16.clone());
    assert_eq!(line.source_range().unwrap(), boundary.source_utf16);
    for (index, glyph) in expected.iter().enumerate() {
        assert_eq!(line.glyph(index).unwrap(), Some(*glyph));
    }
    let mut builder = line.spool_builder().unwrap();
    while line.step_spool(&mut builder, 1).unwrap().status
        != crate::MarkdownShapedSpoolStatus::AwaitingSeal
    {}
    let mut spool = line.seal_spool(builder).unwrap();
    let observer = spool.resource_observer();
    let authority = spool.authority();
    line.close().unwrap(); // The actual child closes first; read-only spool retains its necessary original Source/Font and unique output nonce.
    for (index, glyph) in expected.iter().enumerate() {
        assert_eq!(
            spool
                .glyph(
                    &authority,
                    &fixture.authority,
                    &fixture.font_authority,
                    index
                )
                .unwrap(),
            Some(*glyph)
        );
    }
    assert_eq!(
        spool
            .glyph(
                &authority,
                &fixture.authority,
                &fixture.font_authority,
                expected.len()
            )
            .unwrap(),
        None
    );
    // Actual spool EOF and close are independent from the earlier child's successful glyph
    // comparison; its descriptor must really end with a successful cleanup receipt.
    spool.close().unwrap();
    assert!(
        !observer.snapshot().file_open
            && observer.snapshot().close_succeeded
            && !observer.snapshot().cleanup_failed
    );
}

#[test]
fn continuous_emergency_lines_transfer_real_tail_and_spool_without_fake_unicode_breaks() {
    // Walk LTR/RTL emergency lines using genuine transferred remainder jobs and their private
    // start proofs. Each line still compares every glyph against the independent finite oracle.
    for direction in [
        MarkdownShapingDirection::LeftToRight,
        MarkdownShapingDirection::RightToLeft,
    ] {
        let text = "officexyzabcdefghijkfifififiziabcdeffa\u{301}ghijkxyzabcdef";
        let end = text.encode_utf16().count();
        let mut fixture = Fixture::new(text, 0..end, direction);
        let measure = fixture.measure(0, 2700);
        let (resolver, answer) = fixture.resolve(&measure, tiny());
        assert!(matches!(answer, MarkdownSoftLineResolveAnswer::Resolved(_)));
        let mut observers = vec![resolver.resource_observer()]; // This finite control retains all receipts; production keeps no per-line history Vec.
        let mut parts = resolver
            .take_parts(&measure, &mut fixture.shape)
            .unwrap()
            .into_parts();
        drop(measure); // Later lines cannot borrow the old Measure; the actual transferred remainder carries its unique start proof.
        let mut cursor = 0;
        let mut lines = 0;
        loop {
            let (boundary, line, remainder) = parts;
            assert_eq!(boundary.source_utf16.start, cursor);
            assert!(boundary.source_utf16.end > cursor);
            assert!(boundary.x_advance_26_6 <= 2700);
            // The fixed sample's combining mark must never become an interior line endpoint;
            // this independent literal fact supplements, rather than duplicates, GCB lookup.
            let mark = text.encode_utf16().position(|unit| unit == 0x301).unwrap();
            assert_ne!(boundary.source_utf16.end, mark);
            cursor = boundary.source_utf16.end;
            lines += 1;
            transferred_line_matches_spool_oracle(&fixture, text, &boundary, line);
            let Some(mut remainder) = remainder else {
                break;
            };
            assert_eq!(remainder.source_range().unwrap().start, cursor);
            if cursor < end {
                let mut ordinary = fixture
                    .breaks
                    .cursor(
                        crate::MarkdownLayoutBreakUnit::Utf16,
                        cursor,
                        crate::MarkdownLayoutBreakQuery::At,
                    )
                    .unwrap();
                loop {
                    match fixture.breaks.query(&mut ordinary, 1).unwrap() {
                        crate::MarkdownLayoutBreakAnswer::Known(value) => {
                            assert!(value.is_none());
                            break;
                        }
                        crate::MarkdownLayoutBreakAnswer::Yielded => {}
                        answer => panic!("完整控制源不应未知: {answer:?}"),
                    }
                } // A real emergency start is not a UAX14 opportunity; the next measure uses its sealed GCB proof, not a fabricated At answer.
            }
            let mut clusters = remainder.cluster_index().unwrap();
            // Subsequent measurement/resolution use the actual remainder child and its own
            // cluster index, never the original item's glyph array under copied coordinates.
            while remainder.step_clusters(&mut clusters, 1).unwrap().status
                != MarkdownGlyphClusterStatus::Ready
            {}
            let mut measure = remainder
                .measure(
                    &mut fixture.breaks,
                    2700,
                    MarkdownSoftLineMeasureCancellation::new(),
                )
                .unwrap();
            loop {
                match remainder
                    .step_measure(
                        &mut measure,
                        &mut clusters,
                        &mut fixture.breaks,
                        MarkdownSoftLineMeasureBudget {
                            break_queries: 1,
                            clusters: 1,
                        },
                    )
                    .unwrap()
                    .answer
                {
                    MarkdownSoftLineMeasureAnswer::Selected(_) => break,
                    MarkdownSoftLineMeasureAnswer::Yielded => {}
                    answer => panic!("同源完整tail测量不可伪结束: {answer:?}"),
                }
            }
            let mut resolver = remainder.resolver(&measure).unwrap();
            observers.push(resolver.resource_observer());
            loop {
                match remainder
                    .step_resolver(
                        &mut resolver,
                        &measure,
                        &mut clusters,
                        &mut fixture.breaks,
                        tiny(),
                    )
                    .unwrap()
                    .answer
                {
                    MarkdownSoftLineResolveAnswer::Resolved(_) => break,
                    MarkdownSoftLineResolveAnswer::Yielded => {}
                    answer => panic!("真实连续紧急软行没有词典/字体缺口: {answer:?}"),
                }
            }
            parts = remainder
                .take_resolved_parts(resolver, &measure)
                .unwrap()
                .into_parts();
            drop(measure);
            drop(clusters);
            remainder.close().unwrap(); // The next real owners are transferred; return the old remainder instead of accumulating a line cache.
        }
        assert_eq!(cursor, end);
        assert!(lines > 5);
        drop(fixture);
        // Only after all actual fixture owners end may every retained receipt report Source
        // payload release and balanced child destruction; the peak bound is a Core resource fact.
        for observer in observers {
            let facts = observer.snapshot();
            assert_eq!(facts.active_jobs, 0);
            assert_eq!(facts.jobs_created, facts.jobs_released);
            assert!(facts.source_payload_released && !facts.cleanup_failed);
            assert!(facts.peak_active_jobs <= 2);
        }
    }
}

#[test]
fn transferred_start_rejects_foreign_break_source_and_original_cancel_stays_binding() {
    // A same-byte independent Source cannot supply break authority for the transferred start.
    // Original cancellation still binds the sibling line after the remainder's rejection.
    let mut fixture = Fixture::new("abcdefghijk", 0..11, MarkdownShapingDirection::LeftToRight);
    let measure = fixture.measure(0, 1700);
    let (resolver, _) = fixture.resolve(&measure, tiny());
    let resources = resolver.resource_observer();
    let (_, mut line, mut remainder) = resolver
        .take_parts(&measure, &mut fixture.shape)
        .unwrap()
        .into_parts();
    drop(measure);
    let mut other = Fixture::new("abcdefghijk", 0..11, MarkdownShapingDirection::LeftToRight);
    assert!(
        remainder
            .as_mut()
            .unwrap()
            .measure(
                &mut other.breaks,
                1700,
                MarkdownSoftLineMeasureCancellation::new()
            )
            .is_err()
    );
    assert!(remainder.as_mut().unwrap().glyph(0).is_err()); // A same-byte new Source cannot reauthorize an already rejected remainder.
    fixture.shape.cancellation().request();
    assert!(matches!(
        line.glyph(0),
        Err(MarkdownSoftLineResolveError::Shaping(
            MarkdownShapingError::Cancelled
        ))
    ));
    drop(line);
    drop(remainder);
    drop(other);
    drop(fixture);
    let facts = resources.snapshot();
    assert_eq!(facts.active_jobs, 0);
    assert_eq!(facts.jobs_created, facts.jobs_released);
    assert!(facts.source_payload_released && !facts.cleanup_failed);
}
fn compare(
    fixture: &mut Fixture,
    text: &str,
    measure: &MarkdownSoftLineMeasureJob,
    job: &mut MarkdownSoftLineResolveJob,
    boundary: &MarkdownSoftLineResolvedBoundary,
) {
    // Independently shape both actual ranges, checking complete glyph geometry, flags, EOF,
    // and the prefix advance. Equal boundary coordinates alone do not establish correct shaping.
    for (side, range, count, flags) in [
        (
            MarkdownSoftLineResolvedSide::Line,
            boundary.source_utf16.clone(),
            boundary.line_glyphs,
            boundary.line_flags_union,
        ),
        (
            MarkdownSoftLineResolvedSide::Remainder,
            boundary.remainder_utf16.clone(),
            boundary.remainder_glyphs,
            boundary.remainder_flags_union,
        ),
    ] {
        let expected = oracle(fixture, text, range);
        assert_eq!(count, expected.len());
        assert_eq!(
            flags,
            expected.iter().fold(0, |flags, glyph| flags | glyph.flags)
        );
        for (index, glyph) in expected.iter().enumerate() {
            assert_eq!(
                job.glyph(measure, &mut fixture.shape, side, index).unwrap(),
                Some(*glyph)
            );
        }
        assert_eq!(
            job.glyph(measure, &mut fixture.shape, side, expected.len())
                .unwrap(),
            None
        );
        if side == MarkdownSoftLineResolvedSide::Line {
            assert_eq!(
                boundary.x_advance_26_6,
                expected
                    .iter()
                    .map(|glyph| i64::from(glyph.x_advance))
                    .sum::<i64>()
            );
        }
    }
}

#[test]
fn long_word_emergency_ltr_rtl_and_nonzero_source_match_complete_real_oracle() {
    // Construct, advance, and destroy the entire Core owner graph inside this actual worker;
    // do not move a !Send ShapeJob into another thread after construction.
    std::thread::spawn(|| {
        for direction in [
            MarkdownShapingDirection::LeftToRight,
            MarkdownShapingDirection::RightToLeft,
        ] {
            let text = "前言\nffiabcdefghijk";
            let end = text.encode_utf16().count();
            let mut fixture = Fixture::new(text, 3..end, direction);
            let measure = fixture.measure(3, 2700);
            assert!(matches!(
                measure.resolve_seed(&mut fixture.shape).unwrap().choice,
                MarkdownSoftLineMeasureChoice::NeedsEmergencyBreak(_)
            ));
            let (mut job, answer) = fixture.resolve(&measure, tiny());
            let MarkdownSoftLineResolveAnswer::Resolved(boundary) = answer else {
                panic!("实际长词必须解析: {answer:?}")
            };
            assert!(boundary.source_utf16.start == 3 && boundary.source_utf16.end < end);
            assert_eq!(boundary.kind, MarkdownSoftLineResolvedKind::Emergency);
            assert!(boundary.x_advance_26_6 <= 2700);
            compare(&mut fixture, text, &measure, &mut job, &boundary);
            let resources = job.close().unwrap();
            assert_eq!(resources.active_jobs, 0);
            assert_eq!(resources.jobs_created, resources.jobs_released);
            assert!(!resources.cleanup_failed);
        }
    })
    .join()
    .unwrap();
}

#[test]
fn real_ligature_boundary_inside_cluster_is_reshaped_on_both_sides() {
    // This actual font/input produces a selected boundary inside a ligature cluster. Require
    // that precondition, then compare independently reshaped prefix and remainder glyphs.
    let text = "f\u{200b}fi";
    let mut fixture = Fixture::new(text, 0..4, MarkdownShapingDirection::LeftToRight);
    let measure = fixture.measure(0, 100_000);
    let choice = measure.resolve_seed(&mut fixture.shape).unwrap().choice;
    assert!(
        matches!(
            choice,
            MarkdownSoftLineMeasureChoice::NeedsReshape {
                reason: crate::MarkdownSoftLineReshapeReason::BoundaryInsideCluster,
                ..
            }
        ),
        "必须真实触发cluster内部断点: {choice:?}"
    );
    let (mut job, answer) = fixture.resolve(&measure, tiny());
    let MarkdownSoftLineResolveAnswer::Resolved(boundary) = answer else {
        panic!("实际ligature边界必须解析: {answer:?}")
    };
    assert_eq!(boundary.source_utf16, 0..2);
    compare(&mut fixture, text, &measure, &mut job, &boundary);
}

#[test]
fn real_unsafe_boundary_preserves_actual_flags_and_original_style() {
    // The soft-hyphen case must genuinely carry UnsafeToBreak under the original font/style.
    // No private flag mutation substitutes for actual selection and reshaping.
    let text = "A\u{ad}V";
    let mut fixture = Fixture::new(text, 0..3, MarkdownShapingDirection::LeftToRight);
    let measure = fixture.measure(0, 900);
    let choice = measure.resolve_seed(&mut fixture.shape).unwrap().choice;
    assert!(
        matches!(
            choice,
            MarkdownSoftLineMeasureChoice::NeedsReshape {
                reason: crate::MarkdownSoftLineReshapeReason::UnsafeToBreak,
                ..
            }
        ),
        "必须真实触发unsafe断点: {choice:?}"
    );
    let (mut job, answer) = fixture.resolve(&measure, MarkdownSoftLineResolveBudget::default());
    let MarkdownSoftLineResolveAnswer::Resolved(boundary) = answer else {
        panic!("实际unsafe边界必须解析: {answer:?}")
    };
    compare(&mut fixture, text, &measure, &mut job, &boundary);
}

#[test]
fn giant_single_grapheme_is_not_split_or_claimed_to_fit() {
    // The finite combining sequence is one real GCB wider than the requested line. Preserve
    // its complete coverage and explicit overflow instead of splitting it or claiming it fits.
    let text = format!("a{}", "\u{301}".repeat(4096));
    let mut fixture = Fixture::new(&text, 0..4097, MarkdownShapingDirection::LeftToRight);
    let measure = fixture.measure(0, 1);
    let (mut job, answer) = fixture.resolve(&measure, MarkdownSoftLineResolveBudget::default());
    let MarkdownSoftLineResolveAnswer::Resolved(boundary) = answer else {
        panic!("完整超宽GCB须明确overflow: {answer:?}")
    };
    assert_eq!(boundary.source_utf16, 0..4097);
    assert_eq!(
        boundary.kind,
        MarkdownSoftLineResolvedKind::OverflowingGrapheme
    );
    assert!(boundary.x_advance_26_6 > 1);
    compare(&mut fixture, &text, &measure, &mut job, &boundary);
}

#[test]
fn missing_dictionary_incomplete_item_and_empty_source_have_truthful_phases() {
    // Actual missing-font, dictionary, and next-item cases stay Pending with their typed reason.
    // None may create placeholder child shapes or turn an item edge into paragraph EOF.
    for (text, item, expected) in [
        (
            "🙂",
            0..2,
            MarkdownSoftLineResolvePending::FontFallback {
                source_utf16: 0..2,
                missing_glyphs: 1,
            },
        ),
        ("กข", 0..2, MarkdownSoftLineResolvePending::Dictionary),
        ("abcdef", 0..3, MarkdownSoftLineResolvePending::NextItem),
    ] {
        let mut fixture = Fixture::new(text, item, MarkdownShapingDirection::LeftToRight);
        let measure = fixture.measure(0, 100_000);
        let (job, answer) = fixture.resolve(&measure, tiny());
        assert_eq!(answer, MarkdownSoftLineResolveAnswer::Pending(expected));
        assert_eq!(job.resource_observer().snapshot().jobs_created, 0);
    }
    let mut fixture = Fixture::new("", 0..0, MarkdownShapingDirection::LeftToRight);
    // Only the genuinely empty original Source has the terminal EmptySource result.
    let measure = fixture.measure(0, 1);
    let (job, answer) = fixture.resolve(&measure, tiny());
    assert_eq!(answer, MarkdownSoftLineResolveAnswer::EmptySource);
    assert_eq!(job.close().unwrap().jobs_created, 0);
}

#[test]
fn original_nonce_cancellation_and_backing_errno_revoke_resolved_values() {
    // Equal Source/width/job does not identify a measure. Rejection must retire the old
    // resolver permanently, and original cancellation must also revoke a newly admitted resolver.
    let text = "abcdefghijk";
    let mut fixture = Fixture::new(text, 0..11, MarkdownShapingDirection::LeftToRight);
    let measure = fixture.measure(0, 1700);
    let (mut job, _) = fixture.resolve(&measure, tiny());
    let other = fixture.measure(0, 1700); // Same Source, width, and actual shape still lack the original Measure nonce.
    assert!(matches!(
        job.glyph(
            &other,
            &mut fixture.shape,
            MarkdownSoftLineResolvedSide::Line,
            0
        ),
        Err(MarkdownSoftLineResolveError::Measure(
            MarkdownSoftLineMeasureError::JobMismatch
        ))
    ));
    assert!(
        job.glyph(
            &measure,
            &mut fixture.shape,
            MarkdownSoftLineResolvedSide::Line,
            0
        )
        .is_err()
    );

    let mut cancelled = MarkdownSoftLineResolveJob::new(&measure, &mut fixture.shape).unwrap();
    let resources = cancelled.resource_observer();
    cancelled.cancellation().cancel();
    assert!(
        cancelled
            .step(
                &measure,
                &mut fixture.shape,
                &mut fixture.clusters,
                &mut fixture.breaks,
                tiny()
            )
            .is_err()
    );
    assert_eq!(resources.snapshot().active_jobs, 0);
}

#[test]
fn real_source_reclaim_error_wins_cancel_and_actual_owner_release_is_observed() {
    // Preserve the existing reclaim-failure injection and exercise its production health path.
    // It establishes error precedence under that injected failure, not a spontaneous kernel EIO.
    let mut fixture = Fixture::new("abcdefghijk", 0..11, MarkdownShapingDirection::LeftToRight);
    let measure = fixture.measure(0, 1700);
    let (mut job, _) = fixture.resolve(&measure, tiny());
    let observer = job.resource_observer();
    fixture.source.visit_byte_range(0..1, |_| Ok(())).unwrap();
    fixture.document.fail_next_original_reclaim_for_test();
    assert_eq!(
        fixture.document.reclaim_original_read_windows_for_test(),
        Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
            libc::EIO
        ))
    );
    job.cancellation().cancel();
    assert!(matches!(
        job.step(
            &measure,
            &mut fixture.shape,
            &mut fixture.clusters,
            &mut fixture.breaks,
            tiny()
        ),
        Err(MarkdownSoftLineResolveError::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
    assert_eq!(observer.snapshot().active_jobs, 0);
    assert!(!observer.snapshot().source_payload_released);
    // Resolver child retirement is separate from the still-live fixture's original Source.
    // Only actual final fixture Drop permits the Source-payload receipt to become released.
    drop(job);
    drop(measure);
    drop(fixture);
    assert!(observer.snapshot().source_payload_released);
    assert!(!observer.snapshot().cleanup_failed);
}

#[test]
fn long_original_item_has_fixed_state_and_all_real_glyph_jobs_close() {
    // This finite large Core fixture checks real child ownership and bounded resolver state.
    // Its allocated test String/oracle setup is not a whole-product memory measurement.
    let text = "a".repeat(262_144);
    let mut fixture = Fixture::new(&text, 0..text.len(), MarkdownShapingDirection::LeftToRight);
    let measure = fixture.measure(0, 3500);
    let (job, answer) = fixture.resolve(&measure, MarkdownSoftLineResolveBudget::default());
    assert!(matches!(answer, MarkdownSoftLineResolveAnswer::Resolved(_)));
    let observer = job.resource_observer();
    assert!(observer.snapshot().jobs_created < 16); // A safe long word must not create a fresh shaping task for every candidate prefix.
    assert!(observer.snapshot().peak_active_jobs <= 2);
    let facts = job.close().unwrap();
    assert_eq!(facts.active_jobs, 0);
    assert_eq!(facts.jobs_created, facts.jobs_released);
    drop(measure);
    drop(fixture);
    assert!(observer.snapshot().source_payload_released);
    assert!(!observer.snapshot().cleanup_failed);
}

#[test]
fn original_cancel_interrupts_real_child_shape_and_join_confirms_release() {
    // Preserve the existing child-shaper checkpoint injection to synchronize cancellation inside
    // actual shaping. This deterministic barrier is not a Native event or physical input action.
    use std::sync::{Barrier, mpsc};
    let entered = Arc::new(Barrier::new(2));
    let resume = Arc::new(Barrier::new(2));
    let worker_entered = entered.clone();
    let worker_resume = resume.clone();
    let (send, receive) = mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let mut fixture = Fixture::new("abcdefghijk", 0..11, MarkdownShapingDirection::LeftToRight);
        let measure = fixture.measure(0, 1700);
        let mut resolver = MarkdownSoftLineResolveJob::new(&measure, &mut fixture.shape).unwrap();
        while !matches!(resolver.payload.as_ref().unwrap().stage, Stage::Shape) {
            resolver
                .step(
                    &measure,
                    &mut fixture.shape,
                    &mut fixture.clusters,
                    &mut fixture.breaks,
                    tiny(),
                )
                .unwrap();
        } // Reach the actual fully ingested child first; cancellation before construction would not exercise deep shaping response.
        let once = AtomicBool::new(false);
        resolver
            .payload
            .as_mut()
            .unwrap()
            .current
            .as_mut()
            .unwrap()
            .job()
            .unwrap()
            .resolve_test_checkpoint(Arc::new(move |_| {
                if !once.swap(true, Ordering::AcqRel) {
                    worker_entered.wait();
                    worker_resume.wait();
                } // This existing checkpoint pauses once, without sleeps, Native events, or another Source ticket.
            }));
        send.send((
            resolver.cancellation(),
            resolver.resource_observer(),
            fixture.document.original_backing_observer().unwrap(),
        ))
        .unwrap();
        assert!(matches!(
            resolver.step(
                &measure,
                &mut fixture.shape,
                &mut fixture.clusters,
                &mut fixture.breaks,
                tiny()
            ),
            Err(MarkdownSoftLineResolveError::Shaping(
                MarkdownShapingError::Cancelled
            ))
        ));
        drop(resolver);
        drop(measure);
        drop(fixture);
    });
    let (cancellation, resources, backing) = receive.recv().unwrap();
    entered.wait();
    cancellation.cancel();
    resume.wait();
    worker.join().unwrap(); // Check actual destruction after worker exit; neither a cancellation bit nor barrier arrival substitutes for join.
    let facts = resources.snapshot();
    assert_eq!(facts.active_jobs, 0);
    assert_eq!(facts.jobs_created, facts.jobs_released);
    assert!(facts.source_payload_released && !facts.cleanup_failed);
    assert!(backing.unmap_succeeded() && !backing.failed());
}

// Emergency safe continuation uses the same actual Source/font and independent finite oracle,
// without adding a different product testing entry.
#[cfg(feature = "paged-safe-glyph-slices")]
include!("emergency_tests.rs");

// Explicit prefix-only controls coexist with complete-tail tests and independently check each
// actual delivered line and bounded work; the two APIs retain their distinct result contracts.
#[cfg(feature = "paged-safe-glyph-slices")]
include!("prefix_only_tests.rs");

// The shared selector's existing JobProvider control must match the complete independent oracle.
// Actual Store-directory ownership and original Job release belong to Composer's real Store controls.
#[cfg(feature = "paged-line-compose")]
include!("stored_tests.rs");
