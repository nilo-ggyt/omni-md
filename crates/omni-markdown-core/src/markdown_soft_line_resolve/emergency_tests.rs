// Use the same finite fixture and independent HarfRust oracle. Product continuation exposes
// neither a complete text String/glyph Vec nor a start reconstructed from a public Range.
fn emergency_expected<'a>(
    all: &'a [MarkdownShapedGlyph],
    range: &Range<usize>,
    direction: MarkdownShapingDirection,
) -> &'a [MarkdownShapedGlyph] {
    // Select independent oracle glyphs by actual monotone source clusters in their visual
    // direction, without using any tested safe-slice token's glyph-index interval.
    let (first, end) = if direction == MarkdownShapingDirection::RightToLeft {
        (
            all.partition_point(|glyph| glyph.source_utf16_cluster >= range.end),
            all.partition_point(|glyph| glyph.source_utf16_cluster >= range.start),
        )
    } else {
        (
            all.partition_point(|glyph| glyph.source_utf16_cluster < range.start),
            all.partition_point(|glyph| glyph.source_utf16_cluster < range.end),
        )
    };
    &all[first..end] // Original source clusters, not the tested token's glyph ordinals, determine this expected slice.
}
fn emergency_seal_and_compare(
    fixture: &mut Fixture,
    line: MarkdownEmergencyGlyphSliceLine,
    expected: &[MarkdownShapedGlyph],
) {
    // Consume the actual safe slice into the real spool builder, while checking boundary width
    // and every glyph field against the independently selected complete oracle.
    let (boundary, slice) = line.into_parts();
    let facts = slice.facts();
    assert_eq!(facts.source_utf16, boundary.source_utf16);
    assert_eq!(facts.x_advance_26_6, boundary.x_advance_26_6);
    assert_eq!(boundary.glyphs, expected.len());
    assert_eq!(
        boundary.x_advance_26_6,
        expected
            .iter()
            .map(|glyph| i64::from(glyph.x_advance))
            .sum::<i64>()
    );
    let mut builder =
        crate::MarkdownShapedSpoolBuilder::from_safe_slice(&mut fixture.shape, slice).unwrap();
    while builder.step(&mut fixture.shape, 17).unwrap().status
        != crate::MarkdownShapedSpoolStatus::AwaitingSeal
    {}
    let mut spool = builder.seal(&mut fixture.shape).unwrap();
    let observer = spool.resource_observer();
    let authority = spool.authority();
    let mut actual = [MarkdownShapedGlyph {
        glyph_id: 0,
        source_utf16_cluster: 0,
        flags: 0,
        x_advance: 0,
        y_advance: 0,
        x_offset: 0,
        y_offset: 0,
    }; 256];
    let mut read = 0;
    // Read only the production batch limit at once; the full expected glyph array belongs
    // exclusively to this finite independent control.
    while read < expected.len() {
        let next = (read + 256).min(expected.len());
        spool
            .read_batch(
                &authority,
                &fixture.authority,
                &fixture.font_authority,
                read..next,
                &mut actual[..next - read],
            )
            .unwrap();
        assert_eq!(&actual[..next - read], &expected[read..next]); // Preserve flags and every geometry field from the complete independent oracle.
        read = next;
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
    // Actual descriptor close is independent of correct glyph bytes and logical EOF.
    let resources = observer.snapshot();
    assert!(!resources.file_open && resources.close_succeeded && !resources.cleanup_failed);
}
fn emergency_stream(
    fixture: &mut Fixture,
    text: &str,
    start: usize,
    width: i64,
    expect_overflow: bool,
    combining_pairs: bool,
) -> MarkdownEmergencyGlyphSliceWork {
    // Construct one original full-item oracle, then consume actual safe lines in FIFO order.
    // The iteration guard fails incomplete finite inputs rather than promoting Pending to success.
    let all = oracle(fixture, text, fixture.shape.source_range());
    let original_profile = fixture.shape.profile().unwrap();
    let measure = fixture.measure(start, width);
    let mut job = MarkdownEmergencyGlyphSliceJob::new(&measure, &mut fixture.shape).unwrap();
    let resources = job.resource_observer();
    let mut cursor = start;
    let mut copied = 0;
    let mut lines = 0;
    for _ in 0..2_000_000 {
        let step = job
            .step(
                &measure,
                &mut fixture.shape,
                &mut fixture.clusters,
                &mut fixture.breaks,
                MarkdownSoftLineResolveBudget {
                    candidates: 17,
                    ..MarkdownSoftLineResolveBudget::default()
                },
            )
            .unwrap();
        assert!(step.candidates <= 17 && step.shapes <= 1 && step.input_bytes <= 4096);
        match step.answer {
            MarkdownEmergencyGlyphSliceAnswer::ReusableLine(boundary) => {
                // Literal combining-pair boundaries supplement production GCB validation;
                // an overwide complete GCB is reported honestly rather than sliced to fit.
                assert_eq!(boundary.source_utf16.start, cursor);
                assert!(boundary.source_utf16.end > cursor);
                if combining_pairs {
                    assert_eq!((boundary.source_utf16.start - start) % 2, 0);
                    assert_eq!((boundary.source_utf16.end - start) % 2, 0);
                }
                if expect_overflow {
                    assert_eq!(
                        boundary.kind,
                        MarkdownSoftLineResolvedKind::OverflowingGrapheme
                    );
                }
                let expected =
                    emergency_expected(&all, &boundary.source_utf16, fixture.options.direction);
                let line = job.take_line(&measure, &mut fixture.shape).unwrap();
                emergency_seal_and_compare(fixture, line, expected);
                cursor = boundary.source_utf16.end;
                copied += expected.len();
                lines += 1;
            }
            MarkdownEmergencyGlyphSliceAnswer::PrefixComplete {
                source_utf16_end,
                source_eof,
            } => {
                // Require complete original Source coverage and every oracle glyph exactly once.
                // Safe reuse must preserve the original shape and perform no new input or shapes.
                assert!(source_eof);
                assert_eq!(source_utf16_end, fixture.source.utf16_len());
                assert_eq!(cursor, source_utf16_end);
                assert_eq!(copied, all.len());
                let work = job.work(&measure, &mut fixture.shape).unwrap();
                assert_eq!(work.lines, lines);
                assert_eq!(work.shape_calls, 0);
                assert_eq!(work.new_shape_input_bytes, 0);
                assert!(work.candidates <= work.original_input_scalars + work.lines);
                let after = fixture.shape.profile().unwrap();
                assert_eq!(after.input_scalars, original_profile.input_scalars);
                assert_eq!(after.source_bytes_read, original_profile.source_bytes_read);
                let release = job.close().unwrap();
                assert_eq!(release.jobs_created, 0);
                assert_eq!(release.active_jobs, 0);
                assert_eq!(resources.snapshot().jobs_released, 0);
                return work;
            }
            MarkdownEmergencyGlyphSliceAnswer::Yielded => {}
            other => panic!("安全合成输入未实际复用：{other:?}"),
        }
    }
    panic!("有限长文没有完整覆盖，不把超时或Pending当通过");
}

#[test]
fn emergency_long_unbroken_ascii_work_is_one_original_shape_and_linear_candidates() {
    // Two finite input sizes exercise actual safe continuation and complete output. Printed
    // timing includes the independent oracle and real temporary spools, not native editing work.
    for count in [4096, 8192] {
        let text = format!("前\n{}", "7".repeat(count));
        let mut fixture = Fixture::new(&text, 2..count + 2, MarkdownShapingDirection::LeftToRight);
        let now = std::time::Instant::now();
        let work = emergency_stream(&mut fixture, &text, 2, 5000, false, false);
        assert_eq!(work.original_input_scalars, count);
        assert_eq!(work.original_glyphs, count);
        assert!(work.lines > 256);
        println!(
            "emergency_safe count={count} original_scalars={} new_shapes={} new_bytes={} candidates={} lines={} elapsed_us={}",
            work.original_input_scalars,
            work.shape_calls,
            work.new_shape_input_bytes,
            work.candidates,
            work.lines,
            now.elapsed().as_micros()
        );
    } // This elapsed time is not whole-product latency or memory acceptance.
}
#[test]
fn emergency_rtl_nonzero_source_and_combining_clusters_reuse_without_tail_shapes() {
    // Nonzero global coordinates, RTL visual order, and combining pairs retain the actual
    // original glyph array; no tail shape may be manufactured merely to continue a safe line.
    let text = format!("前\n{}", "0123456789".repeat(512));
    let end = text.encode_utf16().count();
    let mut fixture = Fixture::new(&text, 2..end, MarkdownShapingDirection::RightToLeft);
    emergency_stream(&mut fixture, &text, 2, 5000, false, false);
    let text = format!("前\n{}", "a\u{301}".repeat(1024));
    let end = text.encode_utf16().count();
    let mut fixture = Fixture::new(&text, 2..end, MarkdownShapingDirection::LeftToRight);
    emergency_stream(&mut fixture, &text, 2, 5000, false, true);
}
#[test]
fn emergency_one_huge_grapheme_is_actual_overflow_and_never_sliced() {
    // The entire finite combining chain is one actual GCB. One explicit overflowing line must
    // preserve it rather than produce apparently fitting fragments.
    let text = format!("a{}", "\u{301}".repeat(4096));
    let mut fixture = Fixture::new(&text, 0..4097, MarkdownShapingDirection::LeftToRight);
    let work = emergency_stream(&mut fixture, &text, 0, 1, true, false);
    assert_eq!(work.lines, 1);
    assert_eq!(work.candidates, 1);
}

#[test]
fn emergency_internal_ligature_candidate_keeps_actual_owned_reshape_path() {
    // This real ligature sample selects an interior cluster endpoint. The complete-prefix/tail
    // constructor must actually reshape both sides; safe-array reuse cannot impersonate that work.
    let text = "ffi".repeat(64);
    let mut fixture = Fixture::new(&text, 0..text.len(), MarkdownShapingDirection::LeftToRight);
    let measure = fixture.measure(0, 1);
    let mut job = MarkdownEmergencyGlyphSliceJob::new(&measure, &mut fixture.shape).unwrap();
    let observer = job.resource_observer();
    let boundary = loop {
        let step = job
            .step(
                &measure,
                &mut fixture.shape,
                &mut fixture.clusters,
                &mut fixture.breaks,
                tiny(),
            )
            .unwrap();
        match step.answer {
            MarkdownEmergencyGlyphSliceAnswer::Reshaped(boundary) => break boundary,
            MarkdownEmergencyGlyphSliceAnswer::Yielded => {}
            other => panic!("cluster内部必须实际shape：{other:?}"),
        }
    };
    assert_eq!(boundary.source_utf16, 0..1);
    assert_eq!(
        boundary.kind,
        MarkdownSoftLineResolvedKind::OverflowingGrapheme
    );
    let work = job.work(&measure, &mut fixture.shape).unwrap();
    assert!(work.shape_calls >= 2 && work.new_shape_input_bytes >= text.len());
    let parts = job
        .take_reshaped_parts(&measure, &mut fixture.shape)
        .unwrap();
    let (_, line, tail) = parts.into_parts();
    transferred_line_matches_spool_oracle(&fixture, &text, &boundary, line);
    tail.unwrap().close().unwrap();
    let release = observer.snapshot();
    assert_eq!(release.active_jobs, 0);
    assert_eq!(release.jobs_created, release.jobs_released);
} // This path still shapes an actual tail; it is not evidence of linear total work for every unsafe input.

#[test]
fn emergency_original_measure_and_source_identity_are_not_replaceable() {
    // A second same-source measure and an independent same-byte Source each lack the original
    // selected authority. First rejection must retire this operation rather than allow recapture.
    let text = "7".repeat(128);
    let mut fixture = Fixture::new(&text, 0..text.len(), MarkdownShapingDirection::LeftToRight);
    let measure = fixture.measure(0, 5000);
    let foreign = fixture.measure(0, 5000);
    let mut job = MarkdownEmergencyGlyphSliceJob::new(&measure, &mut fixture.shape).unwrap();
    assert!(
        job.step(
            &foreign,
            &mut fixture.shape,
            &mut fixture.clusters,
            &mut fixture.breaks,
            tiny()
        )
        .is_err()
    );
    assert!(
        job.step(
            &measure,
            &mut fixture.shape,
            &mut fixture.clusters,
            &mut fixture.breaks,
            tiny()
        )
        .is_err()
    );
    job.close().unwrap();
    let mut job = MarkdownEmergencyGlyphSliceJob::new(&measure, &mut fixture.shape).unwrap();
    let mut other = Fixture::new(&text, 0..text.len(), MarkdownShapingDirection::LeftToRight);
    assert!(
        job.step(
            &measure,
            &mut other.shape,
            &mut other.clusters,
            &mut other.breaks,
            tiny()
        )
        .is_err()
    );
    job.close().unwrap();
}
#[test]
fn emergency_cancel_source_errno_and_last_real_mapping_release_are_observed() {
    // Keep the existing reclaim-failure injection, then also cancel. Original backing failure
    // must win both step and close, while actual final Source/mapping release remains observable.
    let text = "7".repeat(8192);
    let mut fixture = Fixture::new(&text, 0..text.len(), MarkdownShapingDirection::LeftToRight);
    let measure = fixture.measure(0, 5000);
    let source = fixture.source.resource_observer();
    let backing = fixture.source.original_backing_observer().unwrap();
    let mut job = MarkdownEmergencyGlyphSliceJob::new(&measure, &mut fixture.shape).unwrap();
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
    assert!(matches!(
        job.close(),
        Err(MarkdownSoftLineResolveError::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
    drop(measure);
    drop(fixture);
    assert!(source.payload_released() && backing.unmap_succeeded() && backing.mapped_bytes() == 0);
    assert_eq!(observer.snapshot().active_jobs, 0);
}

#[test]
#[cfg(target_os = "macos")]
fn emergency_cjk_real_font_keeps_true_uax_limits_without_faking_eof_or_recreating_shape() {
    // Own the finite Chinese file and use the actual read-only system font mapping. This is a
    // Core font/shape control, not AppKit painting, native font-cache release, or product RSS evidence.
    let text = "中".repeat(512);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("紧急中文真实字体.md");
    std::fs::write(&path, &text).unwrap();
    let document = MarkdownDocumentBuffer::open(&path, text.len() as u64 + 65536).unwrap();
    let source = MarkdownLayoutTextSource::capture(&document);
    let authority = source.authority();
    let bytes = crate::MarkdownLayoutFontBytes::capture_file(
        std::path::Path::new("/System/Library/Fonts/Hiragino Sans GB.ttc"),
        512 * 1024 * 1024,
    )
    .unwrap();
    let observer = bytes.resource_observer();
    let font = MarkdownLayoutFont::resolve(bytes.blob(), "HiraginoSansGB-W3", &[]).unwrap();
    let font_authority = font.authority();
    // Preserve actual Hani/zh shaping and paragraph context, then complete the original shape,
    // cluster index, and line-break index once for all independent starting positions.
    let options = MarkdownShapingOptions {
        direction: MarkdownShapingDirection::LeftToRight,
        script: *b"Hani",
        language: Some("zh".into()),
        points: 16.0,
        beginning_of_text: true,
        end_of_text: true,
        features: Vec::new(),
    };
    let mut shape = MarkdownShapingJob::new(
        source.clone(),
        &authority,
        font.clone(),
        &font_authority,
        0..512,
        options.clone(),
    )
    .unwrap();
    fill(&mut shape, &authority, &font_authority);
    let mut clusters =
        MarkdownGlyphClusterIndex::new(&mut shape, &authority, &font_authority).unwrap();
    while clusters.status() != MarkdownGlyphClusterStatus::Ready {
        clusters.step(&mut shape, 17).unwrap();
    }
    let mut breaks = MarkdownLayoutBreakJob::new(source.clone(), &authority).unwrap();
    while !breaks
        .step(&authority, MarkdownLineBreakBudget::default())
        .unwrap()
        .ready
    {}
    let mut fixture = Fixture {
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
    };
    let all = oracle(&fixture, &text, 0..512);
    let mut candidates = 0;
    for start in 0..512 {
        // Each original measure has a genuine CJK break limit. Ending that one-character scope
        // proves whole-Source EOF only for the actual last character, not every prefix.
        let measure = fixture.measure(start, 1);
        let mut job = MarkdownEmergencyGlyphSliceJob::new(&measure, &mut fixture.shape).unwrap();
        loop {
            let step = job
                .step(
                    &measure,
                    &mut fixture.shape,
                    &mut fixture.clusters,
                    &mut fixture.breaks,
                    MarkdownSoftLineResolveBudget::default(),
                )
                .unwrap();
            match step.answer {
                MarkdownEmergencyGlyphSliceAnswer::Yielded => {}
                MarkdownEmergencyGlyphSliceAnswer::ReusableLine(boundary) => {
                    assert_eq!(boundary.source_utf16, start..start + 1);
                    assert_eq!(
                        boundary.kind,
                        MarkdownSoftLineResolvedKind::OverflowingGrapheme
                    );
                    let line = job.take_line(&measure, &mut fixture.shape).unwrap();
                    emergency_seal_and_compare(
                        &mut fixture,
                        line,
                        emergency_expected(
                            &all,
                            &(start..start + 1),
                            MarkdownShapingDirection::LeftToRight,
                        ),
                    );
                    break;
                }
                other => panic!("中文原完整GCB不得伪造安全或EOF：{other:?}"),
            }
        }
        assert!(
            matches!(job.step(&measure, &mut fixture.shape, &mut fixture.clusters,
            &mut fixture.breaks, tiny()).unwrap().answer,
            MarkdownEmergencyGlyphSliceAnswer::PrefixComplete { source_utf16_end, source_eof }
                if source_utf16_end == start + 1 && source_eof == (start == 511))
        );
        let work = job.work(&measure, &mut fixture.shape).unwrap();
        // Reuse consumes no new shape/input; these Core work observations do not replace the
        // independently checked complete glyphs or eventual real font-mapping teardown.
        assert_eq!(work.shape_calls, 0);
        assert_eq!(work.new_shape_input_bytes, 0);
        candidates += work.candidates;
        assert_eq!(job.close().unwrap().jobs_created, 0);
    }
    assert_eq!(candidates, 512);
    assert_eq!(fixture.shape.profile().unwrap().input_scalars, 512);
    drop(fixture);
    drop(bytes);
    // Only actual final font owners may return the mapped bytes and backing descriptor.
    assert!(
        observer.mapping_drop_completed()
            && observer.unmap_succeeded()
            && observer.mapped_bytes() == 0
            && observer.backing_descriptor_closed()
            && !observer.failed()
    );
}

#[test]
fn emergency_clean_cancel_permanently_drops_selector_and_releases_real_sources() {
    // Normal cancellation without injected backing failure permanently retires this selector.
    // Final Source/unmap success requires dropping the actual original measure and fixture too.
    let text = "7".repeat(128);
    let mut fixture = Fixture::new(&text, 0..text.len(), MarkdownShapingDirection::LeftToRight);
    let measure = fixture.measure(0, 5000);
    let source = fixture.source.resource_observer();
    let backing = fixture.source.original_backing_observer().unwrap();
    let mut job = MarkdownEmergencyGlyphSliceJob::new(&measure, &mut fixture.shape).unwrap();
    job.cancellation().cancel();
    assert!(
        job.step(
            &measure,
            &mut fixture.shape,
            &mut fixture.clusters,
            &mut fixture.breaks,
            tiny()
        )
        .is_err()
    );
    assert!(
        job.step(
            &measure,
            &mut fixture.shape,
            &mut fixture.clusters,
            &mut fixture.breaks,
            tiny()
        )
        .is_err()
    );
    assert_eq!(job.close().unwrap().active_jobs, 0);
    drop(measure);
    drop(fixture);
    assert!(
        source.payload_released()
            && backing.unmap_succeeded()
            && backing.mapped_bytes() == 0
            && !backing.failed()
    );
}
