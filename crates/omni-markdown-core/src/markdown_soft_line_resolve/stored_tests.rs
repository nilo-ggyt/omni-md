// 这里借真实JobProvider控制selector共用性；不把它冒称为实际Store目录或其释放证据。
#[test]
fn resolution_permit_prefix_stream_preserves_global_glyphs_without_shaping_tail() {
    for direction in [
        MarkdownShapingDirection::LeftToRight,
        MarkdownShapingDirection::RightToLeft,
    ] {
        let text = format!("前\n{}", "ffi".repeat(128));
        let mut fixture = Fixture::new(&text, 2..2 + 128 * 3, direction);
        // UTF8与UTF16总长不同，真实item取明确UTF16端点，不能用字节长度冒充全局坐标。
        let end = fixture.shape.source_range().end;
        let backing = fixture.document.original_backing_observer().unwrap();
        let source_drop = fixture.source.resource_observer();
        let measure = fixture.measure(2, 1);
        let permit = measure
            .resolution_permit(&mut JobGlyphProvider::new(&mut fixture.shape, None))
            .unwrap();
        let mut stream = stored::MarkdownStoredPrefixResolveJob::new(
            &permit,
            &mut JobGlyphProvider::new(&mut fixture.shape, None),
        )
        .unwrap();
        let resources = stream.resource_observer();
        let mut cursor = 2;
        let mut new_bytes = 0;
        let mut actual_bytes = 0;
        let mut lines = 0;
        loop {
            let step = stream
                .step(
                    &permit,
                    &mut JobGlyphProvider::new(&mut fixture.shape, Some(&mut fixture.clusters)),
                    &mut fixture.breaks,
                    MarkdownSoftLineResolveBudget::default(),
                )
                .unwrap();
            new_bytes += step.input_bytes;
            assert!(step.shapes <= 1 && step.input_bytes <= 4096 && step.glyphs <= 256);
            match step.answer {
                MarkdownEmergencyGlyphSliceAnswer::ShapedPrefix(boundary) => {
                    assert_eq!(boundary.source_utf16.start, cursor);
                    let expected = oracle(&fixture, &text, boundary.source_utf16.clone());
                    let mut provider = JobGlyphProvider::new(&mut fixture.shape, None);
                    let mut prefix = stream.take_signed_prefix(&permit, &mut provider).unwrap();
                    let facts = prefix.facts(&permit, &mut provider).unwrap();
                    assert_eq!(facts.boundary, boundary);
                    assert!(facts.source == fixture.authority);
                    let mut width = 0;
                    let mut flags = 0;
                    for (ordinal, glyph) in expected.iter().enumerate() {
                        assert_eq!(
                            prefix
                                .glyph(&permit, &mut provider, ordinal)
                                .unwrap()
                                .as_ref(),
                            Some(glyph)
                        );
                        width += i64::from(glyph.x_advance);
                        flags |= glyph.flags;
                    }
                    assert_eq!(boundary.glyphs, expected.len());
                    assert_eq!(boundary.x_advance_26_6, width);
                    assert_eq!(boundary.prefix_flags_union, flags);
                    assert!(
                        prefix
                            .glyph(&permit, &mut provider, expected.len())
                            .unwrap()
                            .is_none()
                    );
                    let mut clusters = prefix.cluster_index(&permit, &mut provider).unwrap();
                    while clusters.status() != MarkdownGlyphClusterStatus::Ready {
                        prefix
                            .step_clusters(&permit, &mut provider, &mut clusters, 7)
                            .unwrap();
                    }
                    let cluster = prefix
                        .containing_cluster(
                            &permit,
                            &mut provider,
                            &mut clusters,
                            boundary.source_utf16.start,
                        )
                        .unwrap();
                    assert!(
                        matches!(cluster, MarkdownGlyphClusterAnswer::Known(Some(value))
                        if value.source_utf16 == boundary.source_utf16)
                    );
                    drop(clusters); // 真实child索引先归还；不能只关闭签包就假称全部派生FD已释放。
                    actual_bytes += byte_offset(&text, boundary.source_utf16.end)
                        - byte_offset(&text, boundary.source_utf16.start);
                    let waiting = stream
                        .step(&permit, &mut provider, &mut fixture.breaks, tiny())
                        .unwrap();
                    assert_eq!(
                        waiting.answer,
                        MarkdownEmergencyGlyphSliceAnswer::PendingPrefixConsumer
                    );
                    assert_eq!((waiting.input_bytes, waiting.shapes), (0, 0));
                    assert_eq!(resources.snapshot().active_jobs, 1);
                    prefix.close().unwrap();
                    cursor = boundary.source_utf16.end;
                    lines += 1;
                }
                MarkdownEmergencyGlyphSliceAnswer::PrefixComplete {
                    source_utf16_end, ..
                } => {
                    assert_eq!(source_utf16_end, end);
                    assert_eq!(cursor, end);
                    break;
                }
                MarkdownEmergencyGlyphSliceAnswer::Yielded => {}
                other => panic!("已具备真实依赖，未完成不能冒充成功: {other:?}"),
            }
        }
        assert_eq!(new_bytes, actual_bytes);
        assert_eq!(new_bytes, end - 2); // 此有限ASCII链每行只输入实际prefix，不重读剩余tail。
        assert_eq!(lines, end - 2);
        let facts = stream.close().unwrap();
        assert!(facts.peak_active_jobs <= 2 && !facts.cleanup_failed);
        assert_eq!(facts.jobs_created, facts.jobs_released);
        drop(measure);
        drop(fixture);
        assert!(source_drop.payload_released());
        assert!(backing.unmap_succeeded() && !backing.failed());
    }
}

#[test]
fn resolution_permit_rejects_safe_choice_and_foreign_original_measure() {
    let mut safe = Fixture::new("123 456", 0..7, MarkdownShapingDirection::LeftToRight);
    let measure = safe.measure(0, 100_000);
    assert!(matches!(
        measure.resolution_permit(&mut JobGlyphProvider::new(&mut safe.shape, None),),
        Err(MarkdownSoftLineMeasureError::InvalidRequest)
    ));
    let text = "ffi".repeat(16);
    let mut fixture = Fixture::new(&text, 0..text.len(), MarkdownShapingDirection::LeftToRight);
    let original = fixture.measure(0, 1);
    let foreign = fixture.measure(0, 1);
    let permit = original
        .resolution_permit(&mut JobGlyphProvider::new(&mut fixture.shape, None))
        .unwrap();
    let foreign_permit = foreign
        .resolution_permit(&mut JobGlyphProvider::new(&mut fixture.shape, None))
        .unwrap();
    let mut stream = stored::MarkdownStoredPrefixResolveJob::new(
        &permit,
        &mut JobGlyphProvider::new(&mut fixture.shape, None),
    )
    .unwrap();
    assert!(matches!(
        stream.step(
            &foreign_permit,
            &mut JobGlyphProvider::new(&mut fixture.shape, Some(&mut fixture.clusters)),
            &mut fixture.breaks,
            tiny(),
        ),
        Err(MarkdownSoftLineResolveError::Measure(
            MarkdownSoftLineMeasureError::JobMismatch
        ))
    ));
    assert_eq!(stream.resource_observer().snapshot().active_jobs, 0);
}

#[test]
fn resolution_permit_source_errno_precedes_cancel_and_releases_actual_payloads() {
    let text = "ffi".repeat(16);
    let mut fixture = Fixture::new(&text, 0..text.len(), MarkdownShapingDirection::LeftToRight);
    let backing = fixture.document.original_backing_observer().unwrap();
    let source_drop = fixture.source.resource_observer();
    let measure = fixture.measure(0, 1);
    let permit = measure
        .resolution_permit(&mut JobGlyphProvider::new(&mut fixture.shape, None))
        .unwrap();
    let mut stream = stored::MarkdownStoredPrefixResolveJob::new(
        &permit,
        &mut JobGlyphProvider::new(&mut fixture.shape, None),
    )
    .unwrap();
    let resources = stream.resource_observer();
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
            &permit,
            &mut JobGlyphProvider::new(&mut fixture.shape, None),
            &mut fixture.breaks,
            tiny(),
        ),
        Err(MarkdownSoftLineResolveError::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
    assert_eq!(resources.snapshot().active_jobs, 0);
    drop(stream);
    drop(measure);
    drop(fixture);
    assert!(source_drop.payload_released());
    assert!(backing.unmap_succeeded() && backing.failed()); // 真实失败不被释放成功洗掉。
}

#[test]
fn original_selector_cannot_rebind_to_later_item_of_same_measure() {
    let mut fixture = Fixture::new("abcdef", 0..3, MarkdownShapingDirection::LeftToRight);
    let mut measure = fixture.measure(0, 100_000);
    let mut resolver = MarkdownSoftLineResolveJob::new(&measure, &mut fixture.shape).unwrap();
    let mut options = fixture.options.clone();
    options.beginning_of_text = false;
    options.end_of_text = true;
    let mut next = MarkdownShapingJob::new(
        fixture.source.clone(),
        &fixture.authority,
        fixture.font.clone(),
        &fixture.font_authority,
        3..6,
        options,
    )
    .unwrap();
    fill(&mut next, &fixture.authority, &fixture.font_authority);
    measure
        .continue_item(
            &mut fixture.shape,
            &mut next,
            &fixture.font_authority,
            &mut fixture.breaks,
        )
        .unwrap();
    let mut clusters =
        MarkdownGlyphClusterIndex::new(&mut next, &fixture.authority, &fixture.font_authority)
            .unwrap();
    assert!(matches!(
        resolver.step(
            &measure,
            &mut next,
            &mut clusters,
            &mut fixture.breaks,
            tiny()
        ),
        Err(MarkdownSoftLineResolveError::Measure(
            MarkdownSoftLineMeasureError::JobMismatch
        ))
    ));
    assert_eq!(resolver.resource_observer().snapshot().jobs_created, 0);
    // 原selector只拥有第一项的票；合法continue不授权它用第二项重解释旧选择，即使没有新shape。
}

#[test]
fn signed_prefix_foreign_measure_rejection_drops_real_child_and_cannot_reauthorize() {
    let text = "ffi".repeat(8);
    let mut fixture = Fixture::new(&text, 0..text.len(), MarkdownShapingDirection::LeftToRight);
    let measure = fixture.measure(0, 1);
    let foreign = fixture.measure(0, 1);
    let permit = measure
        .resolution_permit(&mut JobGlyphProvider::new(&mut fixture.shape, None))
        .unwrap();
    let foreign = foreign
        .resolution_permit(&mut JobGlyphProvider::new(&mut fixture.shape, None))
        .unwrap();
    let mut stream = stored::MarkdownStoredPrefixResolveJob::new(
        &permit,
        &mut JobGlyphProvider::new(&mut fixture.shape, None),
    )
    .unwrap();
    loop {
        match stream
            .step(
                &permit,
                &mut JobGlyphProvider::new(&mut fixture.shape, Some(&mut fixture.clusters)),
                &mut fixture.breaks,
                tiny(),
            )
            .unwrap()
            .answer
        {
            MarkdownEmergencyGlyphSliceAnswer::Yielded => {}
            MarkdownEmergencyGlyphSliceAnswer::ShapedPrefix(_) => break,
            other => panic!("真实首个prefix未形成: {other:?}"),
        }
    }
    let resources = stream.resource_observer();
    let mut prefix = stream
        .take_signed_prefix(
            &permit,
            &mut JobGlyphProvider::new(&mut fixture.shape, None),
        )
        .unwrap();
    assert_eq!(resources.snapshot().active_jobs, 1);
    assert!(matches!(
        prefix.require(
            &foreign,
            &mut JobGlyphProvider::new(&mut fixture.shape, None)
        ),
        Err(MarkdownSoftLineResolveError::Measure(
            MarkdownSoftLineMeasureError::JobMismatch
        ))
    ));
    assert_eq!(resources.snapshot().active_jobs, 0); // 真正child已Drop，不能以撤权限标量代替此收据。
    assert!(matches!(
        prefix.glyph(
            &permit,
            &mut JobGlyphProvider::new(&mut fixture.shape, None),
            0
        ),
        Err(MarkdownSoftLineResolveError::Poisoned)
    ));
    stream.close().unwrap();
}
