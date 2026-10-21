//! Observe actual covered resource roles through the production worker;
//! observations themselves retain neither text nor font payloads.
use super::*;

#[test]
fn whole_worker_observes_static_indexes_and_real_transient_job_cluster_cleanup() {
    // Drive real composition through multiple shaped items and retain the final
    // output reader separately from worker-owned indexes and transient jobs.
    let fixture = Fixture::new("office αλφα слово office βητα текст office gamma конец ");
    let source_mapping = fixture.source.original_backing_observer().unwrap();
    let (mut worker, registered) =
        engine::composer(&fixture, 140 * 64, MarkdownLineComposeCancellation::new());
    let resources = worker.worker_resource_observer();
    let initial = resources.snapshot().unwrap();
    // Fixed roles are assembled but have written no records or handed out a
    // reader. Their live Source obligation cannot be replaced by an invented FD.
    assert!(!initial.scripts.unwrap().run_file_open);
    assert!(!initial.graphemes.unwrap().metadata_file_open);
    assert!(!initial.line_breaks.unwrap().metadata_file_open);
    assert!(!initial.writer.unwrap().file_open);
    // Healthy observations do not prove release while actual fixed owners and Source remain live.
    assert!(!initial.actual_resources_released());
    assert!(!resources.is_released());
    assert!(engine::drive(&mut worker, &registered, false) >= 3);
    let output = worker.into_output().unwrap();
    let before_reader_close = resources.snapshot().unwrap();
    assert!(!before_reader_close.scripts.unwrap().run_file_open);
    assert!(!before_reader_close.graphemes.unwrap().metadata_file_open);
    assert!(!before_reader_close.line_breaks.unwrap().metadata_file_open);
    assert!(before_reader_close.writer.unwrap().file_open);
    assert!(before_reader_close.shapes.registered >= 3);
    assert_eq!(before_reader_close.shapes.active, 0);
    assert_eq!(
        before_reader_close.shapes.files_created,
        before_reader_close.shapes.files_closed
    );
    assert_eq!(
        before_reader_close.shapes.mappings_created,
        before_reader_close.shapes.mappings_released
    );
    assert!(before_reader_close.clusters.registered >= 3);
    assert_eq!(before_reader_close.clusters.active, 0);
    output.close().unwrap();
    // The actual writer FD transferred to the sole reader. Only that reader's
    // real close ends this original file obligation.
    assert!(!resources.snapshot().unwrap().writer.unwrap().file_open);
    assert!(!resources.is_released());
    drop((registered, fixture));
    assert!(resources.is_released());
    assert!(resources.snapshot().unwrap().actual_resources_released());
    assert!(source_mapping.unmap_succeeded());
    assert_eq!(source_mapping.mapped_bytes(), 0);
    assert!(!source_mapping.failed());
}

#[test]
fn cancellation_after_real_shape_drops_payload_before_settling_original_resource_claim() {
    // Reach one actual shape before cancelling the same production worker.
    // A cancellation flag alone is not evidence that the shaped payload ended.
    let fixture = Fixture::new("office office office office office office office office");
    let source_mapping = fixture.source.original_backing_observer().unwrap();
    let cancellation = MarkdownLineComposeCancellation::new();
    let (mut worker, registered) = engine::composer(&fixture, 80 * 64, cancellation.clone());
    let resources = worker.worker_resource_observer();
    let mut shaped = false;
    for _ in 0..100_000 {
        let step = worker.step(budget()).unwrap();
        if let MarkdownLineComposeAnswer::NeedStyle(request) = step.answer {
            // Return the real outstanding request with an actual registered font.
            let range = request.source_range();
            worker
                .provide_style(
                    request
                        .respond_registered(range, &registered, style())
                        .unwrap(),
                )
                .unwrap();
        }
        if step.shapes > 0 {
            // Stop only after real shaping work, while its owner is still active.
            shaped = true;
            break;
        }
    }
    assert!(shaped);
    let active = resources.snapshot().unwrap();
    assert_eq!(active.shapes.registered, 1);
    assert_eq!(active.shapes.active, 1);
    assert_eq!(active.shapes.settled, 0);
    cancellation.cancel();
    // The next production step observes cancellation and drops its actual owner
    // before the original observation can settle the released resource facts.
    assert!(matches!(
        worker.step(budget()),
        Err(MarkdownLineComposeError::Cancelled)
    ));
    let closed = resources.snapshot().unwrap();
    assert!(closed.coverage_complete && !closed.observation_failed);
    assert_eq!(closed.shapes.registered, closed.shapes.settled);
    assert_eq!(closed.shapes.active, 0);
    assert_eq!(closed.shapes.files_created, closed.shapes.files_closed);
    assert_eq!(
        closed.shapes.mappings_created,
        closed.shapes.mappings_released
    );
    assert_eq!(closed.shapes.files_open_after_drop, 0);
    assert_eq!(closed.shapes.mapped_bytes_after_drop, 0);
    assert_eq!(closed.shapes.work_bytes_after_drop, 0);
    assert!(closed.shapes.peak_work_bytes_per_owner > 0);
    assert!(!closed.writer.unwrap().file_open);
    drop((worker, registered, fixture));
    assert!(resources.is_released());
    assert!(source_mapping.unmap_succeeded());
    assert_eq!(source_mapping.mapped_bytes(), 0);
}

#[test]
fn cancellation_after_real_paged_bidi_analysis_keeps_its_actual_close_and_cache_receipt() {
    // Explicit embedding and RTL exclude the uniform path. No unrequested glyph
    // needs an invented font response or fabricated Ready result.
    let fixture = Fixture::new("\u{202b}abc אבג\u{202c}");
    let source_mapping = fixture.source.original_backing_observer().unwrap();
    let cancellation = MarkdownLineComposeCancellation::new();
    let (mut worker, registered) = engine::composer(&fixture, 80 * 64, cancellation.clone());
    let resources = worker.worker_resource_observer();
    assert!(matches!(
        worker.step(budget()).unwrap().answer,
        MarkdownLineComposeAnswer::Yielded
    ));
    let opened = resources.snapshot().unwrap().bidi.unwrap();
    assert!(opened.file_created && opened.file_open);
    assert!(opened.cache_bytes > 0);
    // Cancel with a real open bidi file/cache, then inspect its original close
    // outcome after the production worker's failure path retires the payload.
    cancellation.cancel();
    assert!(matches!(
        worker.step(budget()),
        Err(MarkdownLineComposeError::Cancelled)
    ));
    let closed = resources.snapshot().unwrap();
    assert!(closed.coverage_complete);
    let bidi = closed.bidi.unwrap();
    assert!(bidi.file_created && bidi.close_succeeded && !bidi.file_open);
    assert_eq!(bidi.cache_bytes, 0);
    assert!(bidi.close_errno.is_none());
    drop((worker, registered, fixture));
    assert!(resources.is_released());
    assert!(source_mapping.unmap_succeeded());
    assert_eq!(source_mapping.mapped_bytes(), 0);
}

#[cfg(all(feature = "paged-shaping-frontier", target_os = "macos"))]
#[test]
fn frontier_transfers_its_pre_input_claim_with_the_real_child_without_counting_it_twice() {
    // Produce a real visible line with the actual mapped font/source and verify
    // that its original frontier child resources ended before final Source release.
    let (font, font_mapping) = frontier::mono();
    let fixture = Fixture::new(&"hello world ".repeat(1024));
    let source_mapping = fixture.source.original_backing_observer().unwrap();
    let (mut worker, registered) = frontier::worker(&fixture, &font, 80 * 64);
    let resources = worker.worker_resource_observer();
    let line = frontier::first(&mut worker, &registered);
    assert!(line.source_utf16.end > 0);
    let work = worker.frontier_work().unwrap();
    let actual = resources.snapshot().unwrap();
    assert!(actual.coverage_complete && actual.frontier_coverage_complete);
    assert_eq!(actual.shapes.registered, work.parts);
    assert_eq!(actual.shapes.settled, work.parts);
    assert_eq!(actual.shapes.active, 0);
    assert!(actual.shapes.peak_work_bytes_per_owner > 0);
    // A short child must release its actual work allocation, not merely report no FD.
    assert_eq!(actual.shapes.work_bytes_after_drop, 0);
    assert_eq!(actual.shapes.files_created, actual.shapes.files_closed);
    assert_eq!(
        actual.shapes.mappings_created,
        actual.shapes.mappings_released
    );
    assert!(actual.clusters.registered > 0);
    drop((worker, registered, fixture));
    assert!(resources.is_released());
    assert!(source_mapping.unmap_succeeded());
    assert_eq!(source_mapping.mapped_bytes(), 0);
    drop(font);
    assert!(font_mapping.unmap_succeeded());
}

#[cfg(all(feature = "paged-shaping-frontier", target_os = "macos"))]
#[test]
fn internal_frontier_input_failure_or_cancel_keeps_the_pre_ready_actual_cleanup_receipt() {
    // The existing source-failure variant uses a reclaim injection; both
    // variants still construct, read, cancel, and destroy actual child resources.
    let (font, font_mapping) = frontier::mono();
    for source_error in [false, true] {
        let fixture = Fixture::new(&"hello world ".repeat(1024));
        let source_mapping = fixture.source.original_backing_observer().unwrap();
        let catalog = MarkdownLineFontCatalog::new();
        let registered = catalog.register(&font).unwrap();
        let cancellation = MarkdownLineComposeCancellation::new();
        let mut worker = MarkdownLineComposer::new_with_frontier(
            fixture.source.clone(),
            &fixture.source.authority(),
            catalog,
            80 * 64,
            cancellation.clone(),
        )
        .unwrap();
        let resources = worker.worker_resource_observer();
        let mut input_started = false;
        for _ in 0..100_000 {
            // One-byte/scalar work reaches real child input before cancellation,
            // without substituting a precompleted shape or a simulated ready owner.
            let step = worker
                .step(MarkdownLineComposeBudget {
                    bytes: 1,
                    scalars: 1,
                    records: 1,
                    metadata_units: 1,
                })
                .unwrap();
            if let MarkdownLineComposeAnswer::NeedStyle(request) = step.answer {
                // Satisfy only the actual original style request and registered font.
                let range = request.source_range();
                worker
                    .provide_style(
                        request
                            .respond_registered(range, &registered, style())
                            .unwrap(),
                    )
                    .unwrap();
            }
            if worker.frontier_work().unwrap().input_bytes > 0 {
                // Actual source input, rather than a test-side completion flag,
                // selects the cancellation point.
                input_started = true;
                break;
            }
        }
        assert!(input_started);
        let active = resources.snapshot().unwrap();
        assert_eq!(
            (
                active.shapes.registered,
                active.shapes.active,
                active.shapes.settled
            ),
            (1, 1, 0)
        );
        assert!(active.frontier_coverage_complete);
        // An error branch cannot exempt a still-active shape from its real owner obligation.
        assert!(!active.actual_resources_released());
        if source_error {
            // Existing fault control records EIO after real page reclamation;
            // this does not prove that the kernel itself naturally failed msync.
            fixture
                .document
                .text_snapshot()
                .visit_byte_range(0..1, |_| Ok(()))
                .unwrap();
            fixture.document.fail_next_original_reclaim_for_test();
            assert_eq!(
                fixture.document.reclaim_original_read_windows_for_test(),
                Err(crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO))
            );
        }
        cancellation.cancel();
        let failed = worker.step(budget());
        if source_error {
            // The original sticky Source cause retains priority over later cancellation.
            assert!(matches!(
                failed,
                Err(MarkdownLineComposeError::Source(
                    crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
                ))
            ));
        } else {
            // Without source failure, cancellation remains the actual terminal cause.
            assert!(matches!(failed, Err(MarkdownLineComposeError::Cancelled)));
        }
        let closed = resources.snapshot().unwrap();
        assert!(closed.coverage_complete);
        assert_eq!(
            (
                closed.shapes.registered,
                closed.shapes.active,
                closed.shapes.settled
            ),
            (1, 0, 1)
        );
        assert!(closed.shapes.peak_work_bytes_per_owner > 0);
        // Input actually allocated a shaping work block; cancellation must return it.
        assert_eq!(closed.shapes.work_bytes_after_drop, 0);
        assert_eq!(closed.shapes.files_created, closed.shapes.files_closed);
        assert_eq!(
            closed.shapes.mappings_created,
            closed.shapes.mappings_released
        );
        assert_eq!(closed.shapes.mapped_bytes_after_drop, 0);
        assert!(!closed.writer.unwrap().file_open);
        assert_eq!(closed.first_source_errno, source_error.then_some(libc::EIO));
        drop((worker, registered, fixture));
        assert!(source_mapping.unmap_succeeded());
        assert_eq!(source_mapping.mapped_bytes(), 0);
        let actual = resources.snapshot().unwrap();
        assert_eq!(actual.first_source_errno, source_error.then_some(libc::EIO));
        // Actual descriptors/maps are returned while the prior EIO remains an
        // independent fact; healthy-completion permission must still reject it.
        assert!(actual.actual_resources_released());
        assert_eq!(resources.is_released(), !source_error);
    }
    drop(font);
    assert!(font_mapping.unmap_succeeded());
}
