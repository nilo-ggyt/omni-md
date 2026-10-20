//! 真响应与真实GCB表验证端点交接；不制造stage或把事后统计冒充配额。
use super::*;

struct Fixture {
    _directory: tempfile::TempDir,
    document: crate::MarkdownDocumentBuffer,
    source: MarkdownLayoutTextSource,
}
impl Fixture {
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
                MarkdownLineItemAnswer::NeedStyle(request) => return (worker, request),
                MarkdownLineItemAnswer::Yielded => {}
                _ => panic!("尚未回应样式时不能签字形或EOF"),
            }
        }
        panic!("有限夹具未到原Style阶段")
    }
}
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
fn one() -> MarkdownLineComposeBudget {
    MarkdownLineComposeBudget {
        bytes: 1,
        scalars: 1,
        records: 1,
        metadata_units: 1,
    }
}
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
    let payload = worker.payload.as_ref().unwrap();
    assert!(
        matches!(payload.stage, Stage::StyleEndpoint)
            && payload.pending_style.is_some()
            && payload.frontier.is_none()
    );
    assert!(matches!(
        probe.validate(),
        Err(MarkdownLineComposeError::Poisoned)
    ));
    assert_eq!(
        (backing.managed_resident_bytes(), backing.reclaim_attempts()),
        before
    );
    let step = worker.step(one()).unwrap();
    assert_eq!(
        (
            step.bytes,
            step.scalars,
            step.records,
            step.metadata_units,
            step.shapes
        ),
        (0, 0, 0, 1, 0)
    );
    assert!(matches!(step.answer, MarkdownLineItemAnswer::Yielded));
    let payload = worker.payload.as_ref().unwrap();
    assert!(matches!(payload.stage, Stage::Frontier) && payload.pending_style.is_none());
    assert!(!payload.open_script); // 显式样式末端不获得开放Script/独立字体资格。
    assert_eq!(
        (backing.managed_resident_bytes(), backing.reclaim_attempts()),
        before
    );
    drop((probe, worker));
    released(&mapped);
}

#[test]
fn complete_closed_coverage_uses_the_two_precharged_endpoints_without_source_relocation() {
    let fixture = Fixture::new("abc αλφα");
    let cancel = MarkdownLineComposeCancellation::new();
    let (mut worker, request) = fixture.worker(cancel.clone());
    let resources = worker.resources.observer();
    assert_eq!(request.source_range(), 0..4);
    assert_eq!(request.paragraph_range(), 0..8);
    assert!(!worker.payload.as_ref().unwrap().open_script);
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
    let payload = worker.payload.as_ref().unwrap();
    assert!(
        matches!(payload.stage, Stage::ClosedInput)
            && payload.pending_style.is_none()
            && payload.closed_input.is_some()
            && payload.item.is_none()
            && payload.frontier.is_none()
    ); // 完整自然小项只交接票，不在queue_style隐式构造Job或读取原邻接正文。
    assert!(!payload.open_script);
    assert_eq!(
        (backing.managed_resident_bytes(), backing.reclaim_attempts()),
        before
    );
    assert!(matches!(
        probe.validate(),
        Err(MarkdownLineComposeError::Poisoned)
    ));
    assert_eq!(resources.snapshot().unwrap().shapes.registered, 0);
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
        let payload = worker.payload.as_ref().unwrap();
        if context_bytes < 8 {
            assert!(matches!(payload.stage, Stage::ClosedInput) && payload.item.is_none());
        } else {
            assert!(matches!(payload.stage, Stage::Input) && payload.closed_input.is_none());
        } // 后继Greek是原真实8byte/4scalar上下文，不能在1byte配额内一次隐式吞完。
    }
    assert_eq!((context_bytes, context_scalars), (8, 4));
    let job_resources = {
        // profile会核原Job健康，须取得真实可变借用；本块结束后才可取消并推进同一worker。
        let item = worker.payload.as_mut().unwrap().item.as_mut().unwrap();
        let profile = item.job.profile().unwrap();
        assert_eq!(profile.source_bytes_read, 8);
        assert_eq!(profile.input_scalars, 0);
        assert_eq!(profile.source_borrow_max_bytes, 1);
        assert_eq!(item.facts.source_utf16, 0..4);
        assert_eq!(item.facts.paragraph_utf16, 0..8);
        assert!(item.facts.beginning_of_text && !item.facts.end_of_text);
        item.job.resource_observer()
    };
    cancel.cancel();
    assert!(matches!(
        worker.step(one()),
        Err(MarkdownLineComposeError::Cancelled)
    ));
    assert!(worker.payload.is_none());
    let jobs = resources.snapshot().unwrap().shapes;
    assert_eq!((jobs.registered, jobs.settled, jobs.active), (1, 1, 0));
    let released_job = job_resources.snapshot();
    assert_eq!(
        (
            released_job.files_open,
            released_job.mappings_open,
            released_job.mapped_bytes
        ),
        (0, 0, 0)
    );
    assert!(!released_job.cleanup_failed && released_job.first_error.is_none());
    assert!(font_payload.payload_released());
    drop(worker);
    released(&mapped);
}

#[test]
fn public_synchronous_boundary_error_stays_immediate_but_internal_short_request_is_charged_later() {
    let fixture = Fixture::new("abc😀 xyz");
    let (font, mapped) = font();
    let (mut sync, request) = fixture.worker(MarkdownLineComposeCancellation::new());
    assert!(matches!(
        sync.provide_style(response(request, 4, font.clone())),
        Err(MarkdownLineComposeError::InvalidRequest)
    ));
    assert!(sync.payload.is_none()); // UTF16第4位在真实emoji代理对内部，不能由样式范围签票。
    let (mut queued, request) = fixture.worker(MarkdownLineComposeCancellation::new());
    queued.queue_style(response(request, 4, font)).unwrap();
    assert!(queued.payload.as_ref().unwrap().pending_style.is_some());
    assert!(matches!(
        queued.step(one()),
        Err(MarkdownLineComposeError::InvalidRequest)
    ));
    assert!(queued.payload.is_none());
    drop((sync, queued));
    released(&mapped);
}

#[test]
fn pending_response_cancel_source_failure_and_drop_release_the_actual_font_mapping() {
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
                cancel.cancel();
                assert!(matches!(
                    worker.step(one()),
                    Err(MarkdownLineComposeError::Cancelled)
                ));
            }
            1 => {
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
            _ => {} // 直接Drop也必须清掉唯一pending响应，不等待一次多余的step回调。
        }
        drop(worker);
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
    let fixture = Fixture::new("abc😀 xyz");
    let (mut worker, _) = fixture.worker(MarkdownLineComposeCancellation::new());
    let (other, request) = fixture.worker(MarkdownLineComposeCancellation::new());
    let (font, mapped) = font();
    assert!(matches!(
        worker.queue_style(response(request, 2, font)),
        Err(MarkdownLineComposeError::SourceMismatch)
    ));
    assert!(worker.payload.is_none());
    drop((worker, other));
    released(&mapped);
}
