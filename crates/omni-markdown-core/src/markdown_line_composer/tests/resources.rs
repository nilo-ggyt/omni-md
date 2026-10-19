//! 通过正式worker观察全部已覆盖角色；读数本身不保活正文或字体。
use super::*;

#[test]
fn whole_worker_observes_static_indexes_and_real_transient_job_cluster_cleanup() {
    let fixture = Fixture::new("office αλφα слово office βητα текст office gamma конец ");
    let source_mapping = fixture.source.original_backing_observer().unwrap();
    let (mut worker, registered) =
        engine::composer(&fixture, 140 * 64, MarkdownLineComposeCancellation::new());
    let resources = worker.worker_resource_observer();
    let initial = resources.snapshot().unwrap();
    // 固定角色已装配但尚未写记录或交付Reader；不能用虚构FD来代替仍活着的Source所有权。
    assert!(!initial.scripts.unwrap().run_file_open);
    assert!(!initial.graphemes.unwrap().metadata_file_open);
    assert!(!initial.line_breaks.unwrap().metadata_file_open);
    assert!(!initial.writer.unwrap().file_open);
    assert!(!initial.actual_resources_released()); // 真实固定角色和Source仍在，不能只看健康状态。
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
    // writer实际FD已经交给唯一只读reader，只有reader真实close后这张原FD票才归零。
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
    let fixture = Fixture::new("office office office office office office office office");
    let source_mapping = fixture.source.original_backing_observer().unwrap();
    let cancellation = MarkdownLineComposeCancellation::new();
    let (mut worker, registered) = engine::composer(&fixture, 80 * 64, cancellation.clone());
    let resources = worker.worker_resource_observer();
    let mut shaped = false;
    for _ in 0..100_000 {
        let step = worker.step(budget()).unwrap();
        if let MarkdownLineComposeAnswer::NeedStyle(request) = step.answer {
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
fn poisoned_resource_mutex_never_returns_a_fabricated_all_zero_release_snapshot() {
    let fixture = Fixture::new("真实资源观察锁");
    let (worker, registered) =
        engine::composer(&fixture, 80 * 64, MarkdownLineComposeCancellation::new());
    let resources = worker.worker_resource_observer();
    let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        resources.poison_for_test();
    }));
    assert!(poisoned.is_err());
    assert!(resources.snapshot().is_none());
    assert!(!resources.is_released());
    drop((worker, registered, fixture));
    assert!(resources.snapshot().is_none());
    assert!(!resources.is_released());
}

#[test]
fn cancellation_after_real_paged_bidi_analysis_keeps_its_actual_close_and_cache_receipt() {
    // 显式嵌入和RTL字符排除Uniform快路；无需给未请求的glyph伪造字体或Ready。
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

#[cfg(feature = "paged-shaping-frontier")]
#[test]
fn later_successful_frontier_registration_cannot_wash_an_earlier_unobserved_constructor() {
    let fixture = Fixture::new("未观察构造不能恢复");
    let scope = crate::markdown_line_composer::worker_resources::ComposerResourceScope::new(
        &fixture.source,
    );
    let observer = scope.observer();
    // 模拟构造实际返回Err的控制流：原一次性登记先Drop，后继owner只能消费自己的新票。
    drop(scope.begin_frontier_audit());
    let later_scope = scope.begin_frontier_audit().install().unwrap();
    let checked = observer.snapshot().unwrap();
    assert!(!checked.coverage_complete && !checked.frontier_coverage_complete);
    assert!(!observer.is_released());
    drop((later_scope, scope, fixture));
    assert!(observer.snapshot().unwrap().source_payload_released);
    assert!(!observer.snapshot().unwrap().actual_resources_released()); // 覆盖缺口仍拒绝签资源完整归还。
    assert!(!observer.is_released());
}

#[cfg(all(feature = "paged-shaping-frontier", target_os = "macos"))]
#[test]
fn frontier_transfers_its_pre_input_claim_with_the_real_child_without_counting_it_twice() {
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
    assert_eq!(actual.shapes.work_bytes_after_drop, 0); // 短child真实释放工作块，不能用零FD替代owner退场。
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
            let step = worker
                .step(MarkdownLineComposeBudget {
                    bytes: 1,
                    scalars: 1,
                    records: 1,
                    metadata_units: 1,
                })
                .unwrap();
            if let MarkdownLineComposeAnswer::NeedStyle(request) = step.answer {
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
        assert!(!active.actual_resources_released()); // 活动原shape不能由故障分支免除其真实owner义务。
        if source_error {
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
            assert!(matches!(
                failed,
                Err(MarkdownLineComposeError::Source(
                    crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
                ))
            ));
        } else {
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
        assert_eq!(closed.shapes.work_bytes_after_drop, 0); // 输入一个字素已实际分配工作块，取消必须真实归还。
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
        assert!(actual.actual_resources_released()); // FD/map已真实归还；原EIO仍独立保持，健康门仍失败。
        assert_eq!(resources.is_released(), !source_error);
    }
    drop(font);
    assert!(font_mapping.unmap_succeeded());
}
