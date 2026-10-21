//! Stored providers retain last-fitting measurement and two-phase handoff after the original job drops.
use super::*;
use crate::markdown_soft_line_measure::provider::GlyphProvider;
use crate::{
    MarkdownSoftLineMeasureAnswer, MarkdownSoftLineMeasureBudget, MarkdownSoftLineMeasureChoice,
    MarkdownSoftLineMeasureError, MarkdownSoftLineMeasureJob, MarkdownSoftLineMeasureRequest,
};

fn item_until(
    fixture: &Fixture,
    coordinator: &mut MarkdownLineItemCoordinator,
    end: usize,
    points: f32,
) -> MarkdownLineItem {
    // Split actual style coverage at the requested original endpoint; the item
    // producer still decides its genuine script, bidi and shaping boundaries.
    for _ in 0..10000 {
        match coordinator
            .step(MarkdownLineComposeBudget::default())
            .unwrap()
            .answer
        {
            MarkdownLineItemAnswer::NeedStyle(request) => {
                // The next item's registration belongs to this fixture's catalog;
                // a shorter response changes style coverage, not source authority.
                let coverage = request.source_range().start..request.source_range().end.min(end);
                coordinator
                    .provide_style(
                        request
                            .respond_registered(
                                coverage,
                                &fixture.font,
                                MarkdownLineComposeStyle {
                                    presentation: Default::default(),
                                    points,
                                    language: None,
                                    features: Vec::new(),
                                },
                            )
                            .unwrap(),
                    )
                    .unwrap();
            }
            MarkdownLineItemAnswer::ItemReady(_) => return coordinator.take_item().unwrap(),
            MarkdownLineItemAnswer::Yielded => {}
            _ => panic!("实际item在预期样式范围前结束"),
        }
    }
    panic!("有限item没有Ready");
}
pub(super) fn selected(
    measure: &mut MarkdownSoftLineMeasureJob,
    store: &mut GlyphStore,
    proof: &StoredItemProof,
    breaks: &mut crate::MarkdownLayoutBreakJob,
) -> MarkdownSoftLineMeasureChoice {
    // Every quantum borrows the same stored proof. Yielding leaves the measure
    // pending until its real provider and break dependencies select a result.
    for _ in 0..10000 {
        let mut provider = store.borrow(proof).unwrap();
        if let MarkdownSoftLineMeasureAnswer::Selected(choice) = measure
            .step_with_provider(
                &mut provider,
                breaks,
                MarkdownSoftLineMeasureBudget::default(),
            )
            .unwrap()
            .answer
        {
            return choice;
        }
    }
    panic!("有限Stored provider测量没有选中真实机会");
}

#[test]
fn stored_provider_continues_after_old_job_drop_and_selects_last_fit_before_new_item() {
    // Produce adjacent items with different point sizes and retire both original
    // jobs before measuring across their persisted boundary.
    let fixture = Fixture::new("aa bb cc dd");
    let mut coordinator = fixture.coordinator();
    let mut breaks = super::lines::breaks(&fixture);
    let mut store = fixture.store(MarkdownLineComposeCancellation::new());
    let mut first = item_until(&fixture, &mut coordinator, 6, 16.0);
    let first_proof = capture(&mut store, &mut first);
    let first_facts = store.facts(&first_proof).unwrap();
    let first_job = first.job.resource_observer();
    drop(first);
    assert_eq!(first_job.snapshot().files_open, 0);
    let mut second = item_until(&fixture, &mut coordinator, fixture.source.utf16_len(), 22.0);
    let second_proof = capture(&mut store, &mut second);
    let second_job = second.job.resource_observer();
    drop(second);
    assert_eq!(second_job.snapshot().files_open, 0);
    // Derive width from actual stored cluster advances so the next word overflows
    // midway; the expected break must remain the last fit in the previous item.
    let mut word = 0;
    let mut at = 6;
    while at < 9 {
        let cluster = store.cluster(&second_proof, at).unwrap().unwrap();
        word += cluster.x_advance_26_6;
        at = cluster.source_utf16.end;
    }
    assert!(word > 0);
    let width = first_facts.x_advance_26_6 + word / 2;
    let font = fixture.font.font().authority();
    let cancellation = crate::MarkdownSoftLineMeasureCancellation::new();
    let mut measure = {
        let mut provider = store.borrow(&first_proof).unwrap();
        MarkdownSoftLineMeasureJob::new_with_provider(
            fixture.source.clone(),
            &fixture.source.authority(),
            &font,
            &mut provider,
            &mut breaks,
            MarkdownSoftLineMeasureRequest {
                start_utf16: 0,
                width_26_6: width,
                cancellation,
            },
        )
        .unwrap()
    };
    assert!(
        matches!(selected(&mut measure,&mut store,&first_proof,&mut breaks),MarkdownSoftLineMeasureChoice::NeedNextItem{source_utf16,..} if source_utf16== (0..6))
    );
    // The measured-item permit proves only the consumed first range. Prepare its
    // continuation before opening the next provider; neither borrow remains live.
    {
        let mut provider = store.borrow(&first_proof).unwrap();
        let permit = measure.measured_item_permit(&mut provider).unwrap();
        let facts = permit.require(&mut provider).unwrap();
        assert_eq!(facts.source_utf16, 0..6);
        assert_eq!(facts.measured_utf16, 0..6);
    }
    let continuation = {
        let mut previous = store.borrow(&first_proof).unwrap();
        measure.prepare_continue(&mut previous).unwrap()
    };
    {
        let mut next = store.borrow(&second_proof).unwrap();
        continuation
            .continue_with(&mut next, &font, &mut breaks)
            .unwrap();
    }
    // Continuing into a larger style does not force an item boundary into a new
    // line or discard the remembered last-fitting opportunity.
    let choice = selected(&mut measure, &mut store, &second_proof, &mut breaks);
    assert!(
        matches!(choice,MarkdownSoftLineMeasureChoice::MeasuredBreak(selected) if selected.source_utf16==(0..6) && selected.x_advance_26_6==first_facts.x_advance_26_6)
    );
    assert_eq!(store.facts(&first_proof).unwrap().facts.source_utf16, 0..6); // Later lines can revisit the original stored range.
}

#[test]
fn immutable_forest_finds_many_real_items_without_a_resident_item_vector() {
    // Persist many genuinely separate items while retaining only one live job and
    // proof at a time; later lookup must recover each original source interval.
    let fixture = Fixture::new(&"a ".repeat(257));
    let mut coordinator = fixture.coordinator();
    let mut store = fixture.store(MarkdownLineComposeCancellation::new());
    let observer = store.observer();
    for number in 0..257 {
        let mut item = item_until(&fixture, &mut coordinator, (number + 1) * 2, 16.0);
        let proof = capture(&mut store, &mut item);
        assert_eq!(
            store.facts(&proof).unwrap().facts.source_utf16,
            number * 2..(number + 1) * 2
        );
        drop(item); // This iteration also drops its proof; the oracle does not retain N job identities.
        assert!(observer.snapshot().mapped_bytes <= 16 * 1024);
    }
    for offset in (0..fixture.source.utf16_len()).rev() {
        let proof = store.item_at(offset).unwrap().unwrap().proof;
        let facts = store.facts(&proof).unwrap();
        assert_eq!(
            facts.facts.source_utf16,
            (offset / 2) * 2..(offset / 2 + 1) * 2
        );
    }
    assert!(store.item_at(fixture.source.utf16_len()).unwrap().is_none());
    // The real backing file and mapping must be released after the final reverse
    // lookup; an internal directory shape is not a resource-release receipt.
    store.close().unwrap();
    let state = observer.snapshot();
    assert!(!state.file_open && state.mapped_bytes == 0 && !state.cleanup_failed);
}

#[test]
fn stored_selected_permit_builds_mixed_script_lines_across_real_item_boundaries() {
    // This finite oracle persists three actual scripts first and immediately
    // drops each original job. The product has no parallel resident item vector.
    let fixture = Fixture::new("office αλφα слово office βητα текст office gamma конец ");
    let cancellation = MarkdownLineComposeCancellation::new();
    let mut coordinator = fixture.coordinator();
    let mut breaks = super::lines::breaks(&fixture);
    let mut store = fixture.store(cancellation.clone());
    while !store.capture_graphemes(&mut breaks, 7).unwrap() {}
    let mut item_count = 0;
    while let Some(mut item) = fixture.next(&mut coordinator) {
        capture(&mut store, &mut item);
        item_count += 1;
        let observer = item.job.resource_observer();
        drop(item);
        assert_eq!(observer.snapshot().files_open, 0);
    }
    assert!(item_count >= 5);
    drop(coordinator);
    // One real paragraph analysis supplies visual ordering for all stored items;
    // the line measure still owns the last-fitting decision across item boundaries.
    let mut bidi = crate::MarkdownBidiParagraph::analyze_layout_source(
        fixture.source.clone(),
        &fixture.source.authority(),
        0..fixture.source.utf16_len(),
        crate::BidiBaseDirection::Auto,
        crate::BidiMemoryBudget::new(65536).unwrap(),
    )
    .unwrap();
    let font = fixture.font.font().authority();
    let mut start = 0;
    let mut lines = 0;
    let mut crossed_items = false;
    while start < fixture.source.utf16_len() {
        let mut proof = store.item_at(start).unwrap().unwrap().proof;
        let mut measure = {
            let mut provider = store.borrow(&proof).unwrap();
            MarkdownSoftLineMeasureJob::new_with_provider(
                fixture.source.clone(),
                &fixture.source.authority(),
                &font,
                &mut provider,
                &mut breaks,
                MarkdownSoftLineMeasureRequest {
                    start_utf16: start,
                    width_26_6: 140 * 64,
                    cancellation: cancellation.measure(),
                },
            )
            .unwrap()
        };
        // A NeedNextItem response authorizes only the explicit two-phase
        // continuation. A selected result is committed through its original permit.
        loop {
            match selected(&mut measure, &mut store, &proof, &mut breaks) {
                MarkdownSoftLineMeasureChoice::NeedNextItem { source_utf16, .. } => {
                    // End the old provider borrow before opening the next one;
                    // passing a cloned coordinate cannot substitute for continuation.
                    let next = store.item_at(source_utf16.end).unwrap().unwrap().proof;
                    let continuation = {
                        let mut old = store.borrow(&proof).unwrap();
                        measure.prepare_continue(&mut old).unwrap()
                    };
                    {
                        let mut provider = store.borrow(&next).unwrap();
                        continuation
                            .continue_with(&mut provider, &font, &mut breaks)
                            .unwrap();
                    }
                    proof = next;
                }
                MarkdownSoftLineMeasureChoice::MeasuredBreak(selected) => {
                    // Commit exactly the measured range and advance. A line can
                    // contain several script parts without gaining extra source coverage.
                    let permit = {
                        let mut provider = store.borrow(&proof).unwrap();
                        measure.selected_permit(&mut provider).unwrap()
                    };
                    let facts = store
                        .append_selected_line(&proof, &permit, &mut bidi, 0)
                        .unwrap();
                    assert_eq!(facts.source_utf16, start..selected.source_utf16.end);
                    assert_eq!(facts.x_advance_26_6, selected.x_advance_26_6);
                    assert!(facts.source_utf16.end > start);
                    crossed_items |= facts.parts > 1;
                    start = facts.source_utf16.end;
                    lines += 1;
                    break;
                }
                other => panic!("有限mixed-script fixture必须由真实MeasuredBreak结束: {other:?}"),
            }
        }
    }
    assert!(crossed_items && lines >= 3);
    let mut output = store.into_output().unwrap();
    let observer = output.resource_observer();
    let mut line = output.last_line().unwrap();
    let mut expected_end = fixture.source.utf16_len();
    let mut actual = 0;
    // Re-read committed output after producer retirement, checking both contiguous
    // source coverage and cumulative visual origins of the actual stored parts.
    while let Some(current) = line {
        assert_eq!(current.facts().source_utf16.end, expected_end);
        let mut coverage = 0;
        let mut advance = 0;
        for ordinal in 0..current.facts().parts {
            let part = output.part(&current, ordinal).unwrap().unwrap();
            let facts = part.facts();
            assert_eq!(facts.origin_x_26_6, advance);
            assert!(
                facts.script == *b"Latn" || facts.script == *b"Grek" || facts.script == *b"Cyrl"
            );
            output.read_batch(&part, 0..0, &mut []).unwrap();
            let mut offset = facts.source_utf16.start;
            // Cluster queries must stay inside their real part and advance to an
            // actual source endpoint, including script changes within the line.
            while offset < facts.source_utf16.end {
                let MarkdownGlyphClusterAnswer::Known(Some(cluster)) =
                    output.cluster(&part, offset).unwrap()
                else {
                    panic!("已签LineEnd不能缺少真实cluster");
                };
                assert!(cluster.glyphs.end <= facts.glyphs);
                assert!(cluster.prefix_x_26_6 >= 0);
                offset = cluster.source_utf16.end;
            }
            coverage += facts.source_utf16.len();
            advance += facts.x_advance_26_6;
        }
        assert_eq!(coverage, current.facts().source_utf16.len());
        assert_eq!(advance, current.facts().x_advance_26_6);
        expected_end = current.facts().source_utf16.start;
        actual += 1;
        line = output.previous_line(&current).unwrap();
    }
    assert_eq!((actual, expected_end), (lines, 0));
    output.close().unwrap();
    let state = observer.snapshot();
    assert!(
        !state.file_open
            && state.mapped_bytes == 0
            && !state.cleanup_failed
            && !state.reader_failed
    );
}

#[test]
fn stored_provider_cluster_hint_matches_original_index_for_ltr_rtl_and_mixed_jumps() {
    // Real shaping supplies ligatures, combining marks, surrogate pairs and RTL.
    // The comparison index is built independently from the original job.
    let fixture = Fixture::new("office a\u{301} 😀 אבג abc");
    let source_observer = fixture.source.resource_observer();
    let backing = fixture.source.original_backing_observer().unwrap();
    let font_observer = fixture.font.font().resource_observer();
    let mut coordinator = fixture.coordinator();
    let mut store = fixture.store(MarkdownLineComposeCancellation::new());
    let observer = store.observer();
    let source = fixture.source.authority();
    let font = fixture.font.font().authority();
    let mut rtl = false;
    let mut ltr = false;
    while let Some(mut item) = fixture.next(&mut coordinator) {
        let range = item.facts().source_utf16.clone();
        rtl |= item.facts().direction == MarkdownShapingDirection::RightToLeft;
        ltr |= item.facts().direction == MarkdownShapingDirection::LeftToRight;
        let proof = capture(&mut store, &mut item);
        let mut original =
            MarkdownGlyphClusterIndex::new(&mut item.job, &item.authority, &item.font).unwrap();
        while original.step(&mut item.job, 5).unwrap().status != MarkdownGlyphClusterStatus::Ready {
        }
        {
            let mut provider = store.borrow(&proof).unwrap();
            // Repeated and interior UTF-16 queries share one provider borrow;
            // only a real cluster endpoint can advance its sequential hint.
            for offset in range.clone() {
                let expected = original.containing(&mut item.job, offset).unwrap();
                for _ in 0..2 {
                    assert_eq!(
                        GlyphProvider::cluster(&mut provider, &source, &font, offset).unwrap(),
                        expected
                    );
                }
            }
            // Jump backward and reverse traversal to ensure that a hint cannot
            // interpret an arbitrary coordinate as the next serialized record.
            for offset in [range.start, range.end - 1, range.start]
                .into_iter()
                .chain(range.clone().rev())
            {
                assert_eq!(
                    GlyphProvider::cluster(&mut provider, &source, &font, offset).unwrap(),
                    original.containing(&mut item.job, offset).unwrap()
                );
            }
            assert_eq!(
                GlyphProvider::cluster(&mut provider, &source, &font, range.end).unwrap(),
                MarkdownGlyphClusterAnswer::Known(None)
            );
        }
        drop(original);
        drop(item); // Reborrow stored records after the original job and independent index are gone.
        let mut provider = store.borrow(&proof).unwrap();
        assert!(matches!(
            GlyphProvider::cluster(&mut provider, &source, &font, range.start).unwrap(),
            MarkdownGlyphClusterAnswer::Known(Some(_))
        ));
    }
    assert!(rtl && ltr);
    store.close().unwrap();
    let closed = observer.snapshot();
    assert!(!closed.file_open && closed.mapped_bytes == 0 && !closed.cleanup_failed);
    drop(coordinator);
    // Release every strong source/font owner before consulting weak observers of
    // payload lifetime and the actual original mapping.
    drop(fixture);
    assert!(source_observer.payload_released() && font_observer.payload_released());
    assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
}

#[test]
fn stored_provider_cluster_hint_cannot_hide_cancellation_or_expired_font() {
    // First populate a real sequential hint, then revoke either the actual font
    // lifetime or the same cancellation request before repeating that coordinate.
    for expire_font in [false, true] {
        let fixture = Fixture::new("office");
        let mut coordinator = fixture.coordinator();
        let mut item = fixture.next(&mut coordinator).unwrap();
        let cancellation = MarkdownLineComposeCancellation::new();
        let mut store = fixture.store(cancellation.clone());
        let observer = store.observer();
        let proof = capture(&mut store, &mut item);
        let source = fixture.source.authority();
        let font = fixture.font.font().authority();
        let font_observer = fixture.font.font().resource_observer();
        drop(item);
        drop(coordinator);
        {
            let mut provider = store.borrow(&proof).unwrap();
            assert!(matches!(
                GlyphProvider::cluster(&mut provider, &source, &font, 0).unwrap(),
                MarkdownGlyphClusterAnswer::Known(Some(_))
            ));
            if expire_font {
                // A cached glyph cannot bypass the original weak catalog ticket
                // after the last registered font owner has actually been dropped.
                drop(fixture.font);
                assert!(font_observer.payload_released());
            } else {
                cancellation.cancel(); // Even an identical-coordinate cache hit must observe revocation.
            }
            match GlyphProvider::cluster(&mut provider, &source, &font, 0) {
                Err(MarkdownSoftLineMeasureError::Stored(cause)) if expire_font => {
                    assert!(matches!(*cause, Error::FontExpired));
                }
                Err(MarkdownSoftLineMeasureError::Cancelled) if !expire_font => {}
                other => panic!("已填hint没有保留原字体或取消错误: {other:?}"),
            }
        }
        let closed = observer.snapshot();
        assert!(!closed.file_open && closed.mapped_bytes == 0 && !closed.cleanup_failed);
    }
}

#[test]
fn stored_provider_cluster_hint_rejects_foreign_source_with_identical_text() {
    // Equal bytes, ranges and coordinates do not confer the original capture's
    // authority; the already-filled hint must not leak a stored result to it.
    let fixture = Fixture::new("office");
    let foreign = Fixture::new("office");
    let mut coordinator = fixture.coordinator();
    let mut item = fixture.next(&mut coordinator).unwrap();
    let mut store = fixture.store(MarkdownLineComposeCancellation::new());
    let observer = store.observer();
    let proof = capture(&mut store, &mut item);
    let source = fixture.source.authority();
    let font = fixture.font.font().authority();
    drop(item);
    drop(coordinator);
    {
        let mut provider = store.borrow(&proof).unwrap();
        assert!(matches!(
            GlyphProvider::cluster(&mut provider, &source, &font, 0).unwrap(),
            MarkdownGlyphClusterAnswer::Known(Some(_))
        ));
        // Rejection must also close the real stored file rather than returning
        // the earlier cached cluster under the foreign source authority.
        assert!(matches!(
            GlyphProvider::cluster(&mut provider, &foreign.source.authority(), &font, 0),
            Err(MarkdownSoftLineMeasureError::SourceMismatch)
        ));
    }
    let closed = observer.snapshot();
    assert!(!closed.file_open && closed.mapped_bytes == 0 && !closed.cleanup_failed);
}

#[test]
fn stored_provider_cluster_hint_preserves_source_eio_before_cancel_and_foreign_source() {
    // Establish a successful stored read before making the original source fail.
    // Later cancellation and foreign authority must not overwrite that first cause.
    let fixture = Fixture::new("office");
    let foreign = Fixture::new("office");
    let backing = fixture.source.original_backing_observer().unwrap();
    let mut coordinator = fixture.coordinator();
    let mut item = fixture.next(&mut coordinator).unwrap();
    let cancellation = MarkdownLineComposeCancellation::new();
    let mut store = fixture.store(cancellation.clone());
    let observer = store.observer();
    let proof = capture(&mut store, &mut item);
    let source = fixture.source.authority();
    let font = fixture.font.font().authority();
    drop(item);
    drop(coordinator);
    {
        let mut provider = store.borrow(&proof).unwrap();
        assert!(matches!(
            GlyphProvider::cluster(&mut provider, &source, &font, 0).unwrap(),
            MarkdownGlyphClusterAnswer::Known(Some(_))
        ));
        // Pin an original byte, then consume the existing EIO injection through
        // actual reclaim. This exercises the real path, not a natural kernel EIO.
        fixture
            .document
            .text_snapshot()
            .visit_byte_range(0..1, |_| Ok(()))
            .unwrap();
        fixture.document.fail_next_original_reclaim_for_test();
        assert_eq!(
            fixture.document.reclaim_original_read_windows_for_test(),
            Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                libc::EIO
            ))
        );
        cancellation.cancel(); // The first source failure remains authoritative over later revocations.
        for authority in [&foreign.source.authority(), &source] {
            assert!(matches!(
                GlyphProvider::cluster(&mut provider, authority, &font, 0),
                Err(MarkdownSoftLineMeasureError::Source(
                    MarkdownDocumentBufferError::OriginalBackingUnavailable(errno)
                )) if errno == libc::EIO
            ));
        }
    }
    let closed = observer.snapshot();
    assert!(!closed.file_open && closed.mapped_bytes == 0 && !closed.cleanup_failed);
    drop(store);
    drop(fixture);
    assert!(backing.failed()); // Retaining the first cause is separate from actual unmap and FD cleanup.
    assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0);
}
