// These complete Strings/Vecs belong only to finite synthetic oracles. Product continuation
// retains original Source, fixed selector state, and one outstanding actual prefix.
fn prefix_only_stream(
    text: &str,
    item: Range<usize>,
    width: i64,
) -> MarkdownEmergencyGlyphSliceWork {
    // Share the same real production continuation across directional controls, using LTR here.
    prefix_only_stream_direction(text, item, width, MarkdownShapingDirection::LeftToRight)
}
fn prefix_only_stream_direction(
    text: &str,
    item: Range<usize>,
    width: i64,
    direction: MarkdownShapingDirection,
) -> MarkdownEmergencyGlyphSliceWork {
    // The actual original item supplies Source/Font/context authority once. Each delivered
    // prefix is checked independently, while unresolved remainder stays source coordinates only.
    let mut fixture = Fixture::new(text, item.clone(), direction);
    let backing = fixture.document.original_backing_observer().unwrap();
    let source_drop = fixture.source.resource_observer();
    let original = fixture.shape.profile().unwrap();
    let measure = fixture.measure(item.start, width);
    let mut stream =
        MarkdownEmergencyGlyphSliceJob::new_prefix_only(&measure, &mut fixture.shape).unwrap();
    let resources = stream.resource_observer();
    let mut cursor = item.start;
    let mut count = 0;
    let mut actual_source_bytes = 0usize;
    let mut actual_prefix_jobs = 0usize;
    let mut actual_prefix_input_bytes = 0usize;
    let now = std::time::Instant::now();
    let work = loop {
        let step = stream
            .step(
                &measure,
                &mut fixture.shape,
                &mut fixture.clusters,
                &mut fixture.breaks,
                MarkdownSoftLineResolveBudget::default(),
            )
            .unwrap();
        assert!(
            step.input_bytes <= 4096
                && step.shapes <= 1
                && step.glyphs <= 256
                && step.candidates <= 64
        );
        match step.answer {
            MarkdownEmergencyGlyphSliceAnswer::ShapedPrefix(boundary) => {
                // Compare the actual new child's complete glyphs and profile with independent
                // HarfRust output for this exact global prefix, without shaping the remaining tail.
                assert_eq!(boundary.source_utf16.start, cursor);
                assert!(boundary.source_utf16.end > cursor);
                let expected = oracle(&fixture, text, boundary.source_utf16.clone());
                let (actual_boundary, mut prefix) = stream
                    .take_shaped_prefix(&measure, &mut fixture.shape)
                    .unwrap();
                assert_eq!(boundary, actual_boundary);
                assert_eq!(prefix.source_range().unwrap(), boundary.source_utf16);
                assert_eq!(expected.len(), boundary.glyphs);
                let profile = prefix.profile_for_test().unwrap();
                actual_source_bytes += profile.source_bytes_read;
                actual_prefix_jobs += 1;
                actual_prefix_input_bytes += byte_offset(text, boundary.source_utf16.end)
                    - byte_offset(text, boundary.source_utf16.start);
                assert!(profile.source_borrow_max_bytes <= 4096 && profile.shaper_access_certified);
                let mut width = 0i64;
                let mut flags = 0;
                for (ordinal, glyph) in expected.iter().enumerate() {
                    assert_eq!(prefix.glyph(ordinal).unwrap().as_ref(), Some(glyph));
                    width += i64::from(glyph.x_advance);
                    flags |= glyph.flags;
                }
                assert!(prefix.glyph(expected.len()).unwrap().is_none());
                assert_eq!(boundary.x_advance_26_6, width);
                assert_eq!(boundary.prefix_flags_union, flags);
                // A live handed-off child forbids another line job. Do not empty its payload or
                // count take_shaped_prefix ownership transfer as actual destruction.
                let waiting = stream
                    .step(
                        &measure,
                        &mut fixture.shape,
                        &mut fixture.clusters,
                        &mut fixture.breaks,
                        MarkdownSoftLineResolveBudget::default(),
                    )
                    .unwrap();
                assert_eq!(
                    waiting.answer,
                    MarkdownEmergencyGlyphSliceAnswer::PendingPrefixConsumer
                );
                assert_eq!(
                    (waiting.input_bytes, waiting.shapes, waiting.candidates),
                    (0, 0, 0)
                );
                assert_eq!(resources.snapshot().active_jobs, 1);
                prefix.close().unwrap();
                // Actual close settles this child's resources before FIFO continuation can advance.
                assert_eq!(resources.snapshot().active_jobs, 0);
                cursor = boundary.source_utf16.end;
                count += 1;
            }
            MarkdownEmergencyGlyphSliceAnswer::ReusableLine(boundary) => {
                // Mixed fixtures may contain complete safe clusters. Reuse the original array
                // instead of creating a child Job solely to make all outputs look alike.
                assert_eq!(boundary.source_utf16.start, cursor);
                let expected = oracle(&fixture, text, boundary.source_utf16.clone());
                let line = stream.take_line(&measure, &mut fixture.shape).unwrap();
                emergency_seal_and_compare(&mut fixture, line, &expected);
                cursor = boundary.source_utf16.end;
                count += 1;
            }
            MarkdownEmergencyGlyphSliceAnswer::PrefixComplete {
                source_utf16_end,
                source_eof,
            } => {
                assert_eq!(source_utf16_end, item.end);
                assert_eq!(cursor, item.end);
                assert_eq!(source_eof, item.end == fixture.source.utf16_len());
                let work = stream.work(&measure, &mut fixture.shape).unwrap();
                assert_eq!(work.lines, count);
                // These fixtures either overflow on their first complete GCB or use a wholly
                // certified safe line, with no discarded trial shapes. Independently count
                // actual handed-off children and source bytes rather than comparing work fields to themselves.
                assert_eq!(work.shape_calls, actual_prefix_jobs);
                assert_eq!(work.new_shape_input_bytes, actual_prefix_input_bytes);
                let after = fixture.shape.profile().unwrap();
                assert_eq!(after.input_scalars, original.input_scalars);
                assert_eq!(after.source_bytes_read, original.source_bytes_read);
                println!(
                    "prefix_only source_units={} original_scalars={} lines={} new_input_bytes={} actual_prefix_context_input_bytes={} shape_calls={} elapsed_us={}",
                    item.len(),
                    original.input_scalars,
                    count,
                    work.new_shape_input_bytes,
                    actual_source_bytes,
                    work.shape_calls,
                    now.elapsed().as_micros()
                );
                let release = stream.close().unwrap();
                // Step counts cannot establish release. The original resource observer must
                // independently show each concrete child returned and its actual peak ownership.
                assert_eq!(release.active_jobs, 0);
                assert_eq!(release.jobs_created, release.jobs_released);
                assert!(release.peak_active_jobs <= 2 && !release.cleanup_failed);
                break work;
            }
            MarkdownEmergencyGlyphSliceAnswer::Yielded => {}
            other => panic!("真实前缀未完成，不把未知阶段当安全行: {other:?}"),
        }
    };
    assert!(actual_source_bytes <= work.new_shape_input_bytes + work.shape_calls * 40);
    // Context reads remain separately visible above. Actual Source payload and backing return
    // only after the original measure and all finite fixture owners have dropped.
    drop(measure);
    drop(fixture);
    assert!(source_drop.payload_released());
    assert!(backing.unmap_succeeded() && !backing.failed());
    work
}

#[test]
fn prefix_only_four_and_eight_kib_ligature_chains_do_not_shape_any_remaining_tail() {
    // Each overflowing single-character GCB in these finite chains needs one actual prefix
    // shape. Doubling input must not introduce repeated reshaping of the whole remaining tail.
    for repetitions in [1365, 2730] {
        let input = "ffi".repeat(repetitions);
        let text = format!("前\n{input}");
        let work = prefix_only_stream(&text, 2..input.len() + 2, 1);
        assert_eq!(work.lines, input.len());
        assert_eq!(work.shape_calls, input.len());
        assert_eq!(work.new_shape_input_bytes, input.len());
    } // One actual new shape per complete GCB; neither finite size performs repeated full-tail shaping.
}

#[test]
fn prefix_only_combining_clusters_and_safe_digits_preserve_actual_global_glyphs() {
    // A combining mark stays with its base in the actual prefix; safe digits take the existing
    // array-reuse branch with no new child shape or Source ingestion.
    let input = "f\u{301}fi".repeat(64);
    let text = format!("前\n{input}");
    let work = prefix_only_stream(&text, 2..text.encode_utf16().count(), 1);
    assert!(work.lines < work.original_input_scalars); // Combining marks must still be delivered intact with their base.
    assert!(work.new_shape_input_bytes <= input.len());
    let text = "7".repeat(512);
    let work = prefix_only_stream(&text, 0..512, 5000);
    assert_eq!((work.shape_calls, work.new_shape_input_bytes), (0, 0));
}

#[test]
fn prefix_only_rejects_foreign_measure_and_real_source_failure_before_cancel() {
    // First reject a second measure's nonce, then use the existing reclaim-failure injection
    // to check Source-error precedence over cancellation. No spontaneous kernel error is claimed.
    let text = "ffi".repeat(64);
    let mut fixture = Fixture::new(&text, 0..text.len(), MarkdownShapingDirection::LeftToRight);
    let measure = fixture.measure(0, 1);
    let foreign = fixture.measure(0, 1);
    let mut stream =
        MarkdownEmergencyGlyphSliceJob::new_prefix_only(&measure, &mut fixture.shape).unwrap();
    assert!(matches!(
        stream.step(
            &foreign,
            &mut fixture.shape,
            &mut fixture.clusters,
            &mut fixture.breaks,
            tiny()
        ),
        Err(MarkdownSoftLineResolveError::Measure(
            MarkdownSoftLineMeasureError::JobMismatch
        ))
    ));
    assert_eq!(stream.resource_observer().snapshot().active_jobs, 0);
    let mut stream =
        MarkdownEmergencyGlyphSliceJob::new_prefix_only(&measure, &mut fixture.shape).unwrap();
    stream.cancellation().cancel();
    fixture.document.fail_next_original_reclaim_for_test();
    assert_eq!(
        fixture.document.reclaim_original_read_windows_for_test(),
        Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
            libc::EIO
        ))
    );
    assert!(matches!(
        stream.step(
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
    assert_eq!(stream.resource_observer().snapshot().active_jobs, 0);
}

#[test]
fn prefix_only_rtl_ligatures_preserve_visual_order_and_nonzero_source_context() {
    // RTL starts from the same original nonzero Source interval and complete oracle, while
    // genuine safe-cluster reuse may require fewer children than the LTR ligature control.
    let input = "ffi".repeat(128);
    let text = format!("前\n{input}");
    let work = prefix_only_stream_direction(
        &text,
        2..input.len() + 2,
        1,
        MarkdownShapingDirection::RightToLeft,
    );
    assert_eq!(work.lines, input.len());
    assert!(work.shape_calls > 0 && work.shape_calls <= work.lines);
    assert_eq!(work.new_shape_input_bytes, work.shape_calls);
    // Some actual RTL clusters are safely reusable; create no unnecessary children merely to
    // match LTR counts. The shared control independently counts actual child/reuse branches
    // and compares every glyph with the complete HarfRust oracle.
}

#[test]
fn prefix_only_cancellation_revokes_pending_consumer_without_forcing_child_empty() {
    // Form and hand off an actual Ready prefix before cancellation, rather than testing an
    // unconstructed placeholder. The consumer still owns its concrete child after the stream revokes.
    let text = "ffi".repeat(64);
    let mut fixture = Fixture::new(&text, 0..text.len(), MarkdownShapingDirection::LeftToRight);
    let backing = fixture.document.original_backing_observer().unwrap();
    let source_drop = fixture.source.resource_observer();
    let measure = fixture.measure(0, 1);
    let mut stream =
        MarkdownEmergencyGlyphSliceJob::new_prefix_only(&measure, &mut fixture.shape).unwrap();
    loop {
        match stream
            .step(
                &measure,
                &mut fixture.shape,
                &mut fixture.clusters,
                &mut fixture.breaks,
                MarkdownSoftLineResolveBudget::default(),
            )
            .unwrap()
            .answer
        {
            MarkdownEmergencyGlyphSliceAnswer::ShapedPrefix(_) => break,
            MarkdownEmergencyGlyphSliceAnswer::Yielded => {}
            other => panic!("不能在真实前缀形成前取消占位: {other:?}"),
        }
    }
    let (_, prefix) = stream
        .take_shaped_prefix(&measure, &mut fixture.shape)
        .unwrap();
    let resources = stream.resource_observer();
    assert_eq!(resources.snapshot().active_jobs, 1);
    stream.cancellation().cancel();
    assert!(matches!(
        stream.step(
            &measure,
            &mut fixture.shape,
            &mut fixture.clusters,
            &mut fixture.breaks,
            tiny()
        ),
        Err(MarkdownSoftLineResolveError::Measure(
            MarkdownSoftLineMeasureError::Cancelled
                | MarkdownSoftLineMeasureError::Shaping(MarkdownShapingError::Cancelled)
        ))
    ));
    // Cancellation revokes execution authority only. The caller still owns the actual child;
    // its undestroyed resource count cannot be rewritten to zero as if cleanup had occurred.
    assert_eq!(resources.snapshot().active_jobs, 1);
    prefix.close().unwrap();
    assert_eq!(resources.snapshot().active_jobs, 0);
    stream.close().unwrap();
    drop(measure);
    drop(fixture);
    assert!(source_drop.payload_released());
    assert!(backing.unmap_succeeded() && !backing.failed());
}
