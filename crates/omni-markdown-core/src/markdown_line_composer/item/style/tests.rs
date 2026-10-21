//! Actual responses and original grapheme indexes verify endpoint handoff,
//! source work, invalidation, and real font cleanup through production operations.
use super::*;

struct Fixture {
    _directory: tempfile::TempDir,
    document: crate::MarkdownDocumentBuffer,
    source: MarkdownLayoutTextSource,
}
impl Fixture {
    /// Capture a real isolated UTF-8 source through the production document boundary.
    fn new(text: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("队列样式端点.md");
        std::fs::write(&path, text).unwrap();
        let document =
            crate::MarkdownDocumentBuffer::open(&path, text.len() as u64 + 65536).unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        Self {
            _directory: directory,
            document,
            source,
        }
    }
    /// Advance the actual coordinator until it issues its first real style request.
    fn worker(
        &self,
        cancel: MarkdownLineComposeCancellation,
    ) -> (MarkdownLineItemCoordinator, MarkdownLineStyleRequest) {
        let mut worker = MarkdownLineItemCoordinator::new_with_frontier(
            self.source.clone(),
            &self.source.authority(),
            cancel,
        )
        .unwrap();
        for _ in 0..100_000 {
            match worker
                .step(MarkdownLineComposeBudget::default())
                .unwrap()
                .answer
            {
                // The original worker must ask for style before claiming glyph
                // completion; each Yielded answer resumes the same pending source.
                MarkdownLineItemAnswer::NeedStyle(request) => return (worker, request),
                MarkdownLineItemAnswer::Yielded => {}
                _ => panic!("尚未回应样式时不能签字形或EOF"),
            }
        }
        panic!("有限夹具未到原Style阶段")
    }
}
/// Capture and resolve the actual bundled font while preserving its VM/FD release observation.
fn font() -> (
    MarkdownLayoutFont,
    crate::MarkdownLayoutFontBytesResourceObserver,
) {
    let path = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../vendor/harfrust/benches/fonts/Roboto-Regular.ttf"
    ));
    let bytes = crate::MarkdownLayoutFontBytes::capture_file(path, 4 * 1024 * 1024).unwrap();
    let observer = bytes.resource_observer();
    let font = MarkdownLayoutFont::resolve(bytes.blob(), "Roboto-Regular", &[]).unwrap();
    (font, observer)
}
/// Consume the real request with a finite unchanged style and its actual resolved font.
fn response(
    request: MarkdownLineStyleRequest,
    end: usize,
    font: MarkdownLayoutFont,
) -> MarkdownLineStyleResponse {
    request
        .respond(
            0..end,
            font,
            MarkdownLineComposeStyle {
                presentation: Default::default(),
                points: 16.0,
                language: None,
                features: Vec::new(),
            },
        )
        .unwrap()
}
/// Use the existing one-unit work budget to distinguish queued work from synchronous input.
fn one() -> MarkdownLineComposeBudget {
    MarkdownLineComposeBudget {
        bytes: 1,
        scalars: 1,
        records: 1,
        metadata_units: 1,
    }
}
/// Require actual mapping and backing-descriptor release, including cleanup success.
fn released(observer: &crate::MarkdownLayoutFontBytesResourceObserver) {
    assert!(
        observer.mapping_drop_completed()
            && observer.unmap_succeeded()
            && observer.mapped_bytes() == 0
            && observer.backing_descriptor_closed()
            && !observer.failed()
    );
}

#[test]
fn shortened_coverage_waits_for_one_real_metadata_quantum_and_revokes_old_probe_immediately() {
    // Queue a real shortened response after original pages have been reclaimed.
    // Acceptance must immediately invalidate its escaped font probe without
    // rereading the original source just to perform the queued endpoint work.
    let fixture = Fixture::new("abc😀 xyz");
    let (mut worker, request) = fixture.worker(MarkdownLineComposeCancellation::new());
    assert!(request.source_range().end > 2);
    let probe = worker.font_probe(&request).unwrap();
    let (font, mapped) = font();
    fixture
        .document
        .reclaim_original_read_windows_for_test()
        .unwrap();
    let backing = fixture.source.original_backing_observer().unwrap();
    let before = (backing.managed_resident_bytes(), backing.reclaim_attempts());
    assert_eq!(before.0, 0);
    worker.queue_style(response(request, 2, font)).unwrap();
    assert!(matches!(
        probe.validate(),
        Err(MarkdownLineComposeError::Poisoned)
    ));
    assert_eq!(
        (backing.managed_resident_bytes(), backing.reclaim_attempts()),
        before
    );
    let step = worker.step(one()).unwrap();
    // Endpoint work yields normally and keeps the original source pages
    // reclaimed; private stage/slot representation is not part of this outcome.
    assert!(matches!(step.answer, MarkdownLineItemAnswer::Yielded));
    assert_eq!(
        (backing.managed_resident_bytes(), backing.reclaim_attempts()),
        before
    );
    drop((probe, worker));
    released(&mapped);
}

#[test]
fn complete_closed_coverage_uses_the_two_precharged_endpoints_without_source_relocation() {
    // The actual script intersection already certified this complete small
    // item; accepting its response transfers proof without relocating source endpoints.
    let fixture = Fixture::new("abc αλφα");
    let cancel = MarkdownLineComposeCancellation::new();
    let (mut worker, request) = fixture.worker(cancel.clone());
    let resources = worker.resources.observer();
    assert_eq!(request.source_range(), 0..4);
    assert_eq!(request.paragraph_range(), 0..8);
    let probe = worker.font_probe(&request).unwrap();
    let (font, mapped) = font();
    let font_payload = font.resource_observer();
    fixture
        .document
        .reclaim_original_read_windows_for_test()
        .unwrap();
    let backing = fixture.source.original_backing_observer().unwrap();
    let before = (backing.managed_resident_bytes(), backing.reclaim_attempts());
    assert_eq!(before.0, 0);
    worker.queue_style(response(request, 4, font)).unwrap();
    assert_eq!(
        (backing.managed_resident_bytes(), backing.reclaim_attempts()),
        before
    );
    assert!(matches!(
        probe.validate(),
        Err(MarkdownLineComposeError::Poisoned)
    ));
    assert_eq!(resources.snapshot().unwrap().shapes.registered, 0);
    // Greek following context contributes eight actual source bytes/four
    // scalars, one byte per existing work quantum rather than hidden synchronous input.
    let mut context_bytes = 0;
    let mut context_scalars = 0;
    for _ in 0..8 {
        let step = worker.step(one()).unwrap();
        assert_eq!((step.bytes, step.context_bytes), (1, 1));
        assert!(step.scalars <= 1);
        assert_eq!((step.records, step.metadata_units, step.shapes), (0, 0, 0));
        assert!(matches!(step.answer, MarkdownLineItemAnswer::Yielded));
        context_bytes += step.bytes;
        context_scalars += step.scalars;
    }
    assert_eq!((context_bytes, context_scalars), (8, 4));
    // Cancel the actually constructed pending job and inspect its original
    // aggregate FD/map cleanup instead of reading private coordinator stage fields.
    cancel.cancel();
    assert!(matches!(
        worker.step(one()),
        Err(MarkdownLineComposeError::Cancelled)
    ));
    let jobs = resources.snapshot().unwrap().shapes;
    assert_eq!((jobs.registered, jobs.settled, jobs.active), (1, 1, 0));
    assert_eq!(jobs.files_open_after_drop, 0);
    assert_eq!(jobs.mappings_open_after_drop, 0);
    assert_eq!(jobs.mapped_bytes_after_drop, 0);
    assert!(!jobs.cleanup_failed && jobs.first_error.is_none());
    assert!(font_payload.payload_released());
    drop(worker);
    released(&mapped);
}

#[test]
fn public_synchronous_boundary_error_stays_immediate_but_internal_short_request_is_charged_later() {
    // UTF-16 position four splits the actual emoji surrogate pair. The public
    // synchronous entry rejects immediately; queued validation rejects on its next step.
    let fixture = Fixture::new("abc😀 xyz");
    let (font, mapped) = font();
    let (mut sync, request) = fixture.worker(MarkdownLineComposeCancellation::new());
    assert!(matches!(
        sync.provide_style(response(request, 4, font.clone())),
        Err(MarkdownLineComposeError::InvalidRequest)
    ));
    let (mut queued, request) = fixture.worker(MarkdownLineComposeCancellation::new());
    queued.queue_style(response(request, 4, font)).unwrap();
    assert!(matches!(
        queued.step(one()),
        Err(MarkdownLineComposeError::InvalidRequest)
    ));
    drop((sync, queued));
    released(&mapped);
}

#[test]
fn pending_response_cancel_source_failure_and_drop_release_the_actual_font_mapping() {
    // Each independent fixture creates a real retained response/font mapping.
    // Exercise cancellation, existing source-failure injection, and ordinary Drop.
    for cause in 0..3 {
        let fixture = Fixture::new("abc😀 xyz");
        let cancel = MarkdownLineComposeCancellation::new();
        let (mut worker, request) = fixture.worker(cancel.clone());
        let resources = worker.resources.observer();
        let (font, mapped) = font();
        let font_payload = font.resource_observer();
        worker.queue_style(response(request, 2, font)).unwrap();
        assert!(!font_payload.payload_released() && mapped.mapped_bytes() > 0);
        match cause {
            0 => {
                // The same original cancellation scope retires pending font ownership.
                cancel.cancel();
                assert!(matches!(
                    worker.step(one()),
                    Err(MarkdownLineComposeError::Cancelled)
                ));
            }
            1 => {
                // This existing injection follows actual source-page access and
                // reclamation. Its sticky Source cause takes precedence over cancellation.
                fixture.source.visit_byte_range(0..1, |_| Ok(())).unwrap();
                fixture.document.fail_next_original_reclaim_for_test();
                assert!(matches!(
                    fixture.document.reclaim_original_read_windows_for_test(),
                    Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                        libc::EIO
                    ))
                ));
                cancel.cancel();
                assert!(matches!(
                    worker.step(one()),
                    Err(MarkdownLineComposeError::Source(
                        MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
                    ))
                ));
            }
            // Direct destruction must release the original pending response
            // without requiring an extra callback or work step.
            _ => {}
        }
        drop(worker);
        // Real payload/VM/FD release, rather than an empty private slot, is the
        // terminal fact required from all three lifetime endings.
        assert!(font_payload.payload_released());
        released(&mapped);
        let snapshot = resources.snapshot().unwrap();
        assert!(
            !snapshot.graphemes.unwrap().metadata_file_open
                && !snapshot.scripts.unwrap().run_file_open
                && !snapshot.line_breaks.unwrap().metadata_file_open
        );
    }
}

#[test]
fn queued_style_cannot_consume_a_foreign_same_text_nonce() {
    // A second actual coordinator over the same source still issues another
    // nonce; equal text/range cannot authorize consuming the first one's response.
    let fixture = Fixture::new("abc😀 xyz");
    let (mut worker, _) = fixture.worker(MarkdownLineComposeCancellation::new());
    let (other, request) = fixture.worker(MarkdownLineComposeCancellation::new());
    let (font, mapped) = font();
    assert!(matches!(
        worker.queue_style(response(request, 2, font)),
        Err(MarkdownLineComposeError::SourceMismatch)
    ));
    drop((worker, other));
    released(&mapped);
}
