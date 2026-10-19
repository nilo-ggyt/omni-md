//! 有限合成输入的完整HarfRust oracle与真实Source资源控制，不进入Native或产品依赖图。
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
    MarkdownSoftLineResolveBudget {
        input_bytes: 1,
        input_scalars: 1,
        glyphs: 1,
        candidates: 1,
    }
}
fn byte_offset(text: &str, utf16: usize) -> usize {
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

/// 独立完整有限子串oracle：直接公开HarfRust输入与原全局context/feature，不调用Core派生helper。
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
        .collect() // 仅有限测试完整glyph oracle；产品没有Vec输出或全文借用接口。
}

fn transferred_line_matches_spool_oracle(
    fixture: &Fixture,
    text: &str,
    boundary: &MarkdownSoftLineResolvedBoundary,
    mut line: MarkdownSoftLineResolvedShape,
) {
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
    line.close().unwrap(); // 真正子job先释放；只读spool仍持必要的原Source/Font与唯一输出nonce。
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
    spool.close().unwrap();
    assert!(
        !observer.snapshot().file_open
            && observer.snapshot().close_succeeded
            && !observer.snapshot().cleanup_failed
    );
}

#[test]
fn continuous_emergency_lines_transfer_real_tail_and_spool_without_fake_unicode_breaks() {
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
        let mut observers = vec![resolver.resource_observer()]; // 仅有限控制保存所有释放收据；产品不建立行历史Vec。
        let mut parts = resolver
            .take_parts(&measure, &mut fixture.shape)
            .unwrap()
            .into_parts();
        drop(measure); // 后继不得再借旧Measure；显式交接包内的唯一StartProof已经接管权限。
        let mut cursor = 0;
        let mut lines = 0;
        loop {
            let (boundary, line, remainder) = parts;
            assert_eq!(boundary.source_utf16.start, cursor);
            assert!(boundary.source_utf16.end > cursor);
            assert!(boundary.x_advance_26_6 <= 2700);
            // 固定样本唯一组合簇的内部位置绝不能成为软行结束；不是用同一个GCB算法作唯一oracle。
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
                } // 真紧急行首不是UAX14允许机会；下一Measure依靠sealed GCB证明，而非伪At结果。
            }
            let mut clusters = remainder.cluster_index().unwrap();
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
            remainder.close().unwrap(); // 下一对真实owner已交付；旧整形余项必须实际归还，不能积累行缓存。
        }
        assert_eq!(cursor, end);
        assert!(lines > 5);
        drop(fixture);
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
    assert!(remainder.as_mut().unwrap().glyph(0).is_err()); // 相同字节的新Source不能用来接回已失权余项。
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
    // 整个纯Core owner在真实worker构造/推进/析构；不把!Send ShapeJob移动进线程。
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
    let measure = fixture.measure(0, 1);
    let (job, answer) = fixture.resolve(&measure, tiny());
    assert_eq!(answer, MarkdownSoftLineResolveAnswer::EmptySource);
    assert_eq!(job.close().unwrap().jobs_created, 0);
}

#[test]
fn original_nonce_cancellation_and_backing_errno_revoke_resolved_values() {
    let text = "abcdefghijk";
    let mut fixture = Fixture::new(text, 0..11, MarkdownShapingDirection::LeftToRight);
    let measure = fixture.measure(0, 1700);
    let (mut job, _) = fixture.resolve(&measure, tiny());
    let other = fixture.measure(0, 1700); // 相同源、宽度、实际原Shape仍没有原Measure nonce。
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
    drop(job);
    drop(measure);
    drop(fixture);
    assert!(observer.snapshot().source_payload_released);
    assert!(!observer.snapshot().cleanup_failed);
}

#[test]
fn long_original_item_has_fixed_state_and_all_real_glyph_jobs_close() {
    let text = "a".repeat(262_144);
    let mut fixture = Fixture::new(&text, 0..text.len(), MarkdownShapingDirection::LeftToRight);
    let measure = fixture.measure(0, 3500);
    let (job, answer) = fixture.resolve(&measure, MarkdownSoftLineResolveBudget::default());
    assert!(matches!(answer, MarkdownSoftLineResolveAnswer::Resolved(_)));
    let observer = job.resource_observer();
    assert!(observer.snapshot().jobs_created < 16); // 安全长词不能为每个前缀重建整形任务。
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
        } // 必须真进入已输入完成的子job，不能在构造前取消后冒称深层算法已响应。
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
                } // 原实际checkpoint只停一次；不使用睡眠时序、Native事件或第二源票。
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
    worker.join().unwrap(); // 真正退出之后核实际析构；请求位与barrier到达都不能替代join。
    let facts = resources.snapshot();
    assert_eq!(facts.active_jobs, 0);
    assert_eq!(facts.jobs_created, facts.jobs_released);
    assert!(facts.source_payload_released && !facts.cleanup_failed);
    assert!(backing.unmap_succeeded() && !backing.failed());
}

// 紧急安全续行复用同一真实Source/字体/独立有限oracle，不另造产品测试入口。
#[cfg(feature = "paged-safe-glyph-slices")]
include!("emergency_tests.rs");

// 显式prefix-only流保留旧完整tail测试，并独立核每条真实新行和有限工作量。
#[cfg(feature = "paged-safe-glyph-slices")]
include!("prefix_only_tests.rs");

// 共用selector受控provider入口仍须与原完整oracle一致；真实Store/旧Job释放由composer验。
#[cfg(feature = "paged-line-compose")]
include!("stored_tests.rs");
