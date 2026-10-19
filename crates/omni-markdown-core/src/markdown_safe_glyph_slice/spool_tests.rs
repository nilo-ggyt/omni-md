// 此文件在同一合成测试模块内include，复用真实Source/字体/oracle，不重复生产算法。
struct Historical {
    fixture: Fixture,
    measure: MarkdownSoftLineMeasureJob,
    proof: Option<MarkdownMeasuredItemSpoolProof>,
    old: MarkdownShapedSpool,
    other: MarkdownShapedSpool,
    expected: Vec<MarkdownShapedGlyph>,
}
impl Historical {
    fn new(text: &str, split: usize, width: i64, direction: MarkdownShapingDirection) -> Self {
        let mut fixture = Fixture::new(text, 0..split, direction);
        let expected = oracle(&fixture, text, 0..split);
        let mut measure = fixture.measure(0, width);
        let old = whole_spool(&mut fixture);
        let other = whole_spool(&mut fixture); // 同Job第二个真正文件，故原Job输出票相同但实例必须不同。
        let mut old = old;
        let proof = MarkdownMeasuredItemSpoolProof::capture(&measure, &mut fixture.shape, &mut old)
            .unwrap();
        let old_resources = fixture.shape.resource_observer();
        // 实际后项使用独立Font payload；同名字也不能拿新Font重签历史item。
        let next_font =
            MarkdownLayoutFont::resolve(Arc::new(ROBOTO), "Roboto-Regular", &[]).unwrap();
        let next_authority = next_font.authority();
        let mut options = fixture.options.clone();
        options.beginning_of_text = false;
        options.end_of_text = true;
        let end = fixture.source.utf16_len();
        let mut next = MarkdownShapingJob::new(
            fixture.source.clone(),
            &fixture.authority,
            next_font.clone(),
            &next_authority,
            split..end,
            options.clone(),
        )
        .unwrap();
        fill(&mut next, &fixture.authority, &next_authority);
        measure
            .continue_item(
                &mut fixture.shape,
                &mut next,
                &next_authority,
                &mut fixture.breaks,
            )
            .unwrap();
        fixture.shape = next; // 旧Job实际析构，收据不允许把它藏在历史Vec中继续保活。
        let released = old_resources.snapshot();
        assert_eq!(released.files_open, 0);
        assert_eq!(released.mappings_open, 0);
        assert!(!released.cleanup_failed);
        fixture.font = next_font;
        fixture.font_authority = next_authority;
        fixture.options = options;
        fixture.clusters = MarkdownGlyphClusterIndex::new(
            &mut fixture.shape,
            &fixture.authority,
            &fixture.font_authority,
        )
        .unwrap();
        while fixture.clusters.status() != MarkdownGlyphClusterStatus::Ready {
            fixture.clusters.step(&mut fixture.shape, 13).unwrap();
        }
        loop {
            let answer = measure
                .step(
                    &mut fixture.shape,
                    &mut fixture.clusters,
                    &mut fixture.breaks,
                    MarkdownSoftLineMeasureBudget::default(),
                )
                .unwrap()
                .answer;
            if matches!(answer, MarkdownSoftLineMeasureAnswer::Selected(_)) {
                break;
            }
            assert!(!matches!(
                answer,
                MarkdownSoftLineMeasureAnswer::PendingDependencies
            ));
        }
        Self {
            fixture,
            measure,
            proof: Some(proof),
            old,
            other,
            expected,
        }
    }
    fn job(&mut self) -> MarkdownSafeSpoolSliceJob {
        MarkdownSafeSpoolSliceJob::new(
            &self.measure,
            &mut self.fixture.shape,
            &mut self.old,
            self.proof.take().unwrap(),
        )
        .unwrap()
    }
    fn finish(
        &mut self,
        mut job: MarkdownSafeSpoolSliceJob,
        budget: usize,
    ) -> MarkdownSafeSpoolSlice {
        for _ in 0..200_000 {
            let step = job
                .step(
                    &self.measure,
                    &mut self.fixture.shape,
                    &mut self.old,
                    &mut self.fixture.breaks,
                    budget,
                )
                .unwrap();
            assert!(step.glyphs <= budget);
            assert!(!step.pending_dependencies);
            if step.ready {
                return job
                    .seal(&self.measure, &mut self.fixture.shape, &mut self.old)
                    .unwrap();
            }
        }
        panic!("有限合成item没有完成，不能把超时冒充Ready");
    }
}
fn whole_spool(fixture: &mut Fixture) -> MarkdownShapedSpool {
    let mut builder = MarkdownShapedSpoolBuilder::new(
        &mut fixture.shape,
        &fixture.authority,
        &fixture.font_authority,
    )
    .unwrap();
    while builder.step(&mut fixture.shape, 17).unwrap().status
        != MarkdownShapedSpoolStatus::AwaitingSeal
    {}
    builder.seal(&mut fixture.shape).unwrap()
}
fn empty() -> MarkdownShapedGlyph {
    MarkdownShapedGlyph {
        glyph_id: 0,
        source_utf16_cluster: 0,
        flags: 0,
        x_advance: 0,
        y_advance: 0,
        x_offset: 0,
        y_offset: 0,
    }
}

#[test]
fn history_actual_old_font_and_job_receipt_survives_continue_and_views_exact_ltr_rtl_prefix() {
    for direction in [
        MarkdownShapingDirection::LeftToRight,
        MarkdownShapingDirection::RightToLeft,
    ] {
        let mut history = Historical::new("01234 56789 01234", 9, 5000, direction);
        let job = history.job();
        let slice = history.finish(job, 1);
        let facts = slice.facts();
        assert_eq!(facts.source_utf16, 0..6);
        let expected: Vec<_> = history
            .expected
            .iter()
            .copied()
            .filter(|g| facts.source_utf16.contains(&g.source_utf16_cluster))
            .collect();
        assert_eq!(
            facts.x_advance_26_6,
            expected.iter().map(|g| i64::from(g.x_advance)).sum::<i64>()
        );
        let observer = history.old.resource_observer();
        let original_instance = history.old.instance_identity();
        {
            let mut view = slice.view(&mut history.old).unwrap();
            for (index, expected) in expected.iter().enumerate() {
                let mut actual = [empty()];
                assert_eq!(view.read_batch(index..index + 1, &mut actual).unwrap(), 1);
                assert_eq!(actual[0], *expected); // 全部七字段，包括原UNSAFE_TO_CONCAT原样保留。
            }
            assert_eq!(
                view.read_batch(expected.len()..expected.len(), &mut [])
                    .unwrap(),
                0
            );
        }
        history.old.require_instance(&original_instance).unwrap();
        assert!(observer.snapshot().file_open); // View Drop归还借用，不能谎报原文件已经close。
        history.old.close().unwrap();
        assert!(observer.snapshot().close_succeeded);
    }
}

#[test]
fn history_same_job_other_spool_instance_cannot_replace_captured_file() {
    let mut history = Historical::new(
        "01234 56789 01234",
        9,
        5000,
        MarkdownShapingDirection::LeftToRight,
    );
    let proof = history.proof.take().unwrap();
    assert!(matches!(
        MarkdownSafeSpoolSliceJob::new(
            &history.measure,
            &mut history.fixture.shape,
            &mut history.other,
            proof
        ),
        Err(MarkdownSafeSpoolSliceError::Spool(
            MarkdownShapedSpoolError::OutputMismatch
        ))
    ));
}

#[test]
fn history_completed_slice_cannot_be_borrowed_from_same_job_second_file() {
    let mut history = Historical::new(
        "01234 56789 01234",
        9,
        5000,
        MarkdownShapingDirection::LeftToRight,
    );
    let job = history.job();
    let slice = history.finish(job, 7);
    assert!(matches!(
        slice.view(&mut history.other),
        Err(MarkdownSafeSpoolSliceError::Spool(
            MarkdownShapedSpoolError::OutputMismatch
        ))
    ));
}

#[test]
fn history_cannot_capture_another_job_or_already_final_measure_as_old_item() {
    let text = "01234 56789 01234";
    let mut fixture = Fixture::new(text, 0..9, MarkdownShapingDirection::LeftToRight);
    let measure = fixture.measure(0, 5000);
    let mut wrong = MarkdownShapingJob::new(
        fixture.source.clone(),
        &fixture.authority,
        fixture.font.clone(),
        &fixture.font_authority,
        0..9,
        fixture.options.clone(),
    )
    .unwrap();
    fill(&mut wrong, &fixture.authority, &fixture.font_authority);
    let mut builder =
        MarkdownShapedSpoolBuilder::new(&mut wrong, &fixture.authority, &fixture.font_authority)
            .unwrap();
    while builder.step(&mut wrong, 11).unwrap().status != MarkdownShapedSpoolStatus::AwaitingSeal {}
    let mut wrong = builder.seal(&mut wrong).unwrap();
    assert!(matches!(
        MarkdownMeasuredItemSpoolProof::capture(&measure, &mut fixture.shape, &mut wrong),
        Err(MarkdownSafeSpoolSliceError::Spool(
            MarkdownShapedSpoolError::OutputMismatch
        ))
    ));
    let mut fixture = Fixture::new(text, 0..text.len(), MarkdownShapingDirection::LeftToRight);
    let final_measure = fixture.measure(0, 5000);
    let mut whole = whole_spool(&mut fixture);
    assert!(matches!(
        MarkdownMeasuredItemSpoolProof::capture(&final_measure, &mut fixture.shape, &mut whole),
        Err(MarkdownSafeSpoolSliceError::NeedsResolution)
    ));
}

#[test]
fn history_huge_single_cluster_flags_and_advances_are_finitely_read_without_extra_file() {
    let text = format!("a{} 01234", "\u{301}".repeat(4096));
    let mut history = Historical::new(
        &text,
        4098,
        100_000_000,
        MarkdownShapingDirection::LeftToRight,
    );
    let mut job = history.job();
    let mut reads = 0;
    let mut steps = 0;
    loop {
        let step = job
            .step(
                &history.measure,
                &mut history.fixture.shape,
                &mut history.old,
                &mut history.fixture.breaks,
                1,
            )
            .unwrap();
        assert!(step.glyphs <= 1 && !step.pending_dependencies);
        reads += step.glyphs;
        steps += 1;
        if step.ready {
            break;
        }
        assert!(steps < 100_000);
    }
    assert!(reads >= 4096 && steps >= 4096); // 真实长cluster不在单次调用内悄悄扫完。
    let slice = job
        .seal(
            &history.measure,
            &mut history.fixture.shape,
            &mut history.old,
        )
        .unwrap();
    assert_eq!(slice.facts().source_utf16, 0..4098);
    let facts = slice.facts();
    let mut view = slice.view(&mut history.old).unwrap();
    let mut actual = [empty(); 17];
    let mut read = 0;
    while read < facts.visual_glyphs.len() {
        let next = (read + 17).min(facts.visual_glyphs.len());
        view.read_batch(read..next, &mut actual[..next - read])
            .unwrap();
        assert_eq!(&actual[..next - read], &history.expected[read..next]);
        read = next;
    }
}

#[test]
fn history_measure_cancel_and_drop_revoke_pending_job_or_completed_view() {
    let mut history = Historical::new(
        "01234 56789 01234",
        9,
        5000,
        MarkdownShapingDirection::LeftToRight,
    );
    let job = history.job();
    let slice = history.finish(job, 17);
    let cancel = history
        .measure
        .resolve_seed(&mut history.fixture.shape)
        .unwrap()
        .cancellation;
    let mut view = slice.view(&mut history.old).unwrap();
    cancel.cancel();
    assert!(matches!(
        view.read_batch(0..1, &mut [empty()]),
        Err(MarkdownSafeSpoolSliceError::Cancelled)
    ));
    drop(view);
    let mut history = Historical::new(
        "01234 56789 01234",
        9,
        5000,
        MarkdownShapingDirection::LeftToRight,
    );
    let job = history.job();
    let slice = history.finish(job, 17);
    drop(history.measure);
    assert!(matches!(
        slice.view(&mut history.old),
        Err(MarkdownSafeSpoolSliceError::Poisoned)
    ));
}

#[test]
fn history_zero_budget_failure_is_permanent_and_source_error_precedes_cancel() {
    let mut history = Historical::new(
        "01234 56789 01234",
        9,
        5000,
        MarkdownShapingDirection::LeftToRight,
    );
    let mut job = history.job();
    assert!(
        job.step(
            &history.measure,
            &mut history.fixture.shape,
            &mut history.old,
            &mut history.fixture.breaks,
            0
        )
        .is_err()
    );
    assert!(matches!(
        job.step(
            &history.measure,
            &mut history.fixture.shape,
            &mut history.old,
            &mut history.fixture.breaks,
            17
        ),
        Err(MarkdownSafeSpoolSliceError::Poisoned)
    ));
    let mut history = Historical::new(
        "01234 56789 01234",
        9,
        5000,
        MarkdownShapingDirection::LeftToRight,
    );
    let job = history.job();
    let slice = history.finish(job, 17);
    let cancellation = history
        .measure
        .resolve_seed(&mut history.fixture.shape)
        .unwrap()
        .cancellation;
    history
        .fixture
        .source
        .visit_byte_range(0..1, |_| Ok(()))
        .unwrap();
    history
        .fixture
        .document
        .fail_next_original_reclaim_for_test();
    assert_eq!(
        history
            .fixture
            .document
            .reclaim_original_read_windows_for_test(),
        Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
            libc::EIO
        ))
    );
    cancellation.cancel();
    assert!(matches!(
        slice.view(&mut history.old),
        Err(MarkdownSafeSpoolSliceError::Spool(
            MarkdownShapedSpoolError::Source(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
            )
        ))
    ));
}
