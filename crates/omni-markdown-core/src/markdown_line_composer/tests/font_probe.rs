//! Original real dependencies issue font trials for one complete grapheme only.
//! External copies do not prolong the producing style phase's authority.

use super::*;

#[test]
fn font_probe_preserves_actual_cluster_script_direction_and_source_without_new_indexes() {
    for text in ["a\u{301}b", "中文段落", "👩🏽‍💻正文", "אבג"] {
        let fixture = Fixture::new(text);
        let mut coordinator = fixture.coordinator(MarkdownLineComposeCancellation::new());
        let pending = request(&mut coordinator);
        let resources = coordinator
            .resource_scope()
            .observer()
            .snapshot()
            .expect("实际原依赖已登记，必须取得真实资源收据");
        let probe = coordinator.font_probe(&pending).unwrap();
        let options = probe.shaping_options(&style()).unwrap();
        let expected_end = match text {
            "a\u{301}b" => 2,
            "中文段落" => 1,
            "👩🏽‍💻正文" => "👩🏽‍💻".encode_utf16().count(),
            "אבג" => 1,
            _ => unreachable!(),
        };
        assert_eq!(probe.source_range(), 0..expected_end);
        assert!(probe.accepts_request(&pending).unwrap());
        assert!(probe.source().unwrap().authority() == fixture.source.authority());
        assert_eq!(options.script, pending.script());
        assert_eq!(
            options.beginning_of_text,
            pending.paragraph_range().start == 0
        );
        assert_eq!(
            options.end_of_text,
            expected_end == pending.paragraph_range().end
        );
        assert_eq!(
            options.direction,
            if pending.level() & 1 == 0 {
                crate::MarkdownShapingDirection::LeftToRight
            } else {
                crate::MarkdownShapingDirection::RightToLeft
            }
        );
        // Probe metadata queries must reuse actual original grapheme/Script/bidi
        // resources. Compare real file/mapping observations without a replacement index.
        let after = coordinator
            .resource_scope()
            .observer()
            .snapshot()
            .expect("字体探测不能撤销或伪造原资源收据");
        let script_facts = |value: Option<crate::MarkdownScriptResourceSnapshot>| {
            value.map(|value| {
                (
                    value.run_file_open,
                    value.stack_file_open,
                    value.mapped_bytes,
                    value.run_peak_mapped_bytes,
                    value.stack_peak_mapped_bytes,
                )
            })
        };
        assert_eq!(script_facts(resources.scripts), script_facts(after.scripts));
        assert_eq!(resources.graphemes, after.graphemes);
        assert_eq!(resources.bidi, after.bidi);
    }
}

#[test]
fn font_probe_external_request_and_probe_clones_cannot_extend_consumed_style_scope() {
    // Consume the real response while both external probe copies remain alive;
    // neither equal coordinates nor cloned requests may renew that expired scope.
    let fixture = Fixture::new("office");
    let mut coordinator = fixture.coordinator(MarkdownLineComposeCancellation::new());
    let pending = request(&mut coordinator);
    let probe = coordinator.font_probe(&pending).unwrap();
    let clone = probe.clone();
    let range = pending.source_range();
    coordinator
        .provide_style(pending.clone().respond(range, font(), style()).unwrap())
        .unwrap();
    for stale in [&probe, &clone] {
        assert!(matches!(
            stale.validate(),
            Err(MarkdownLineComposeError::Poisoned)
        ));
        assert!(stale.source().is_err());
        assert!(stale.accepts_request(&pending).is_err());
    }
}

#[test]
fn font_probe_rejects_equal_range_foreign_nonce_and_cancellation() {
    // Two genuine coordinators on the same Source still issue distinct requests.
    // Original shared cancellation must also invalidate the already issued probe.
    let fixture = Fixture::new("same");
    let cancellation = MarkdownLineComposeCancellation::new();
    let mut coordinator = fixture.coordinator(cancellation.clone());
    let pending = request(&mut coordinator);
    let probe = coordinator.font_probe(&pending).unwrap();
    let mut other = fixture.coordinator(MarkdownLineComposeCancellation::new());
    let foreign = request(&mut other);
    assert!(!probe.accepts_request(&foreign).unwrap());
    cancellation.cancel();
    assert!(matches!(
        probe.validate(),
        Err(MarkdownLineComposeError::Cancelled)
    ));
}

#[test]
fn font_probe_is_send_and_scope_drop_revokes_without_retaining_coordinator() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<MarkdownLineFontProbe>();
    let fixture = Fixture::new("worker");
    let mut coordinator = fixture.coordinator(MarkdownLineComposeCancellation::new());
    let pending = request(&mut coordinator);
    let probe = coordinator.font_probe(&pending).unwrap();
    let observer = fixture.source.resource_observer();
    let backing = fixture.document.original_backing_observer().unwrap();
    drop(coordinator);
    drop(fixture);
    assert!(!observer.payload_released()); // The in-flight real packet still owns its Source read claim.
    std::thread::spawn(move || {
        // The actual worker thread receives the stale packet, observes original
        // scope revocation, then releases its final real Source claim before join.
        assert!(matches!(
            probe.validate(),
            Err(MarkdownLineComposeError::Poisoned)
        ));
        drop(probe);
    })
    .join()
    .unwrap();
    assert!(observer.payload_released());
    assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
}

#[test]
fn font_probe_source_error_precedes_expired_scope_and_rejects_oversized_style_before_copy() {
    let fixture = Fixture::new("source fault");
    let mut coordinator = fixture.coordinator(MarkdownLineComposeCancellation::new());
    let pending = request(&mut coordinator);
    let probe = coordinator.font_probe(&pending).unwrap();
    let mut invalid = style();
    invalid.language = Some("a".repeat(65));
    assert!(matches!(
        probe.shaping_options(&invalid),
        Err(MarkdownLineComposeError::InvalidRequest)
    ));
    fixture
        .document
        .text_snapshot()
        .visit_byte_range(0..1, |_| Ok(()))
        .unwrap();
    // Existing injection reports EIO through actual original backing reclaim, not
    // a naturally induced kernel failure. It must win even after scope retirement.
    fixture.document.fail_next_original_reclaim_for_test();
    assert_eq!(
        fixture.document.reclaim_original_read_windows_for_test(),
        Err(crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO))
    );
    drop(coordinator);
    assert!(matches!(
        probe.validate(),
        Err(MarkdownLineComposeError::Source(
            crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
}

#[cfg(all(
    feature = "paged-shaping-frontier",
    feature = "s2-table-window-prototype"
))]
fn prepend_font() -> MarkdownLayoutFont {
    // Use unchanged Amiri 1.002 with its required OFL fixture. A PUA font's missing
    // glyph rectangle cannot prove real Arabic/Prepend coverage.
    let bytes = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fonts/amiri/Amiri-Regular.ttf"
    ));
    let name = read_fonts::FontRef::from_index(bytes, 0)
        .unwrap()
        .table_data(read_fonts::types::Tag::new(b"name"))
        .unwrap()
        .as_bytes()
        .to_vec();
    MarkdownLayoutFont::resolve_name_table_at_size(Arc::new(bytes.as_slice()), &name, &[], 16.0)
        .unwrap()
}

#[cfg(all(
    feature = "paged-shaping-frontier",
    feature = "s2-table-window-prototype"
))]
fn trial_actual_segments(
    worker: &mut MarkdownLineComposer,
    probe: &mut MarkdownLineFontProbe,
    candidate: &MarkdownLayoutFont,
    levels: &[unicode_bidi::Level],
) -> (bool, Vec<std::ops::Range<usize>>) {
    let source = probe.source().unwrap().authority();
    let font = candidate.authority();
    let one = MarkdownLineComposeBudget {
        bytes: 1,
        scalars: 1,
        records: 1,
        metadata_units: 1,
    };
    let mut spans: Vec<std::ops::Range<usize>> = Vec::new(); // Only the fixed short oracle retains prior segments.
    for _ in 0..1024 {
        if !probe.shaping_segment_ready().unwrap() {
            // Metadata preparation stays in the original main-thread composer.
            // Charge its actual work once; only Ready authorizes the following Job.
            // Pending creates no input buffer, glyph file, or successful font result.
            let step = worker.step_font_probe_segment(probe, one).unwrap();
            assert!(
                step.bytes <= 1
                    && step.scalars <= 1
                    && step.records <= 1
                    && step.metadata_units <= 1
            );
            match step.answer {
                MarkdownLineFontProbeSegmentAnswer::Pending => continue,
                MarkdownLineFontProbeSegmentAnswer::Complete => {
                    // Complete traverses this same enclosing grapheme. Accept the
                    // candidate only if every prior real Job had no glyph zero.
                    assert_eq!(spans.first().unwrap().start, probe.source_range().start);
                    assert_eq!(spans.last().unwrap().end, probe.source_range().end);
                    return (true, spans);
                }
                MarkdownLineFontProbeSegmentAnswer::Ready => {}
            }
        }
        let options = probe.shaping_options(&style()).unwrap();
        // Every segment trials the same actual candidate payload. Only original
        // bidi direction/range varies; release each Job before advancing the worker
        // rather than combine two retained glyph graphs into a false completion.
        let mut job = probe
            .shaping_job(
                candidate.clone(),
                &font,
                options.clone(),
                crate::MarkdownShapingCancellation::default(),
            )
            .unwrap();
        let range = job.source_range();
        assert!(range.start >= probe.source_range().start && range.end <= probe.source_range().end);
        // An independent finite complete-UBA oracle preserves each actual level.
        // Equal parity alone cannot merge two original shaping segments.
        let level = levels[range.start];
        assert!(levels[range.clone()].iter().all(|actual| *actual == level));
        assert_eq!(
            options.direction,
            if level.is_rtl() {
                crate::MarkdownShapingDirection::RightToLeft
            } else {
                crate::MarkdownShapingDirection::LeftToRight
            }
        );
        let resources = job.resource_observer();
        // One-byte input splits U+0600/Arabic UTF-8. The original Job retains its
        // one decoder remainder across quanta, without replacing partial scalars.
        while job.status() == crate::MarkdownShapingStatus::Ingesting {
            let step = job
                .step(
                    &source,
                    &font,
                    crate::MarkdownShapingBudget {
                        bytes: 1,
                        scalars: 1,
                    },
                )
                .unwrap();
            assert!(step.bytes <= 1 && step.scalars <= 1);
        }
        job.shape(&source, &font).unwrap();
        let mut missing = false;
        // Inspect each actual glyph, including real .notdef, through original
        // Source/font/EOF checks before deciding whether this candidate covers it.
        for ordinal in 0..job.profile().unwrap().glyph_count {
            let glyph = job.glyph(&source, &font, ordinal).unwrap().unwrap();
            assert!(range.contains(&glyph.source_utf16_cluster));
            missing |= glyph.glyph_id == 0;
        }
        spans.push(range);
        drop(job); // Actual Job release precedes advancing or retrying another candidate.
        let released = resources.snapshot();
        // These actual Job FD/mapping observations prove component release only,
        // not whole-product kernel RSS or platform glyph-cache acceptance.
        assert_eq!(
            (
                released.files_open,
                released.mappings_open,
                released.mapped_bytes
            ),
            (0, 0, 0)
        );
        assert!(!released.cleanup_failed && released.first_error.is_none());
        if missing {
            return (false, spans); // Actual glyph zero rejects the candidate before Complete.
        }
        let next = loop {
            // Reuse this candidate and original request for all segments. Yielding
            // advances only current metadata, never skips unshaped segments.
            let step = worker.step_font_probe_segment(probe, one).unwrap();
            assert!(
                step.bytes <= 1
                    && step.scalars <= 1
                    && step.records <= 1
                    && step.metadata_units <= 1
            );
            if step.answer != MarkdownLineFontProbeSegmentAnswer::Pending {
                break step.answer;
            }
        };
        if next == MarkdownLineFontProbeSegmentAnswer::Complete {
            assert_eq!(spans.last().unwrap().end, probe.source_range().end);
            return (true, spans);
        }
    }
    panic!("固定短字素的同候选真实试次未结束");
}

#[cfg(all(
    feature = "paged-shaping-frontier",
    feature = "s2-table-window-prototype"
))]
#[test]
fn actual_font_trial_checks_every_bidi_segment_before_accepting_one_complete_grapheme() {
    for text in ["\u{600}  ", "\u{600}ب ", "\u{600}  \nnext"] {
        let fixture = Fixture::new(text);
        let backing = fixture.document.original_backing_observer().unwrap();
        let source = fixture.source.resource_observer();
        let catalog = MarkdownLineFontCatalog::new();
        let actual = prepend_font();
        let registered = catalog.register(&actual).unwrap();
        // The production paged entry uses the same visible-paragraph UBA owner
        // even without tables; do not replace it with a complete pre-read path.
        let mut worker = MarkdownLineComposer::new_with_table_flow(
            fixture.source.clone(),
            &fixture.source.authority(),
            catalog,
            10_000 * 64,
            MarkdownLineComposeCancellation::new(),
        )
        .unwrap();
        let resources = worker.worker_resource_observer();
        let pending = loop {
            match worker.step(budget()).unwrap().answer {
                MarkdownLineComposeAnswer::Yielded => {}
                MarkdownLineComposeAnswer::NeedStyle(request) => break request,
                _ => panic!("真实首字体请求之前不能冒充正文已准备"),
            }
        };
        let mut probe = worker.font_probe(&pending).unwrap();
        // Two actual levels belong to one editing grapheme. Its complete original
        // range remains 0..2 while a worker borrows only the current shaping segment.
        assert_eq!(probe.source_range(), 0..2);
        assert!(!probe.complete_single_segment().unwrap());
        assert!(matches!(
            probe.shaping_options(&style()),
            Err(MarkdownLineComposeError::InvalidRequest)
        ));
        let units: Vec<_> = text.encode_utf16().collect();
        // Only this finite synthetic string uses a complete independent UBA oracle.
        // The actual production owner above remains its paged visible-paragraph index.
        let bidi = unicode_bidi::utf16::BidiInfo::new(&units, None);
        let (covered, _) = trial_actual_segments(&mut worker, &mut probe, &font(), &bidi.levels);
        assert!(
            !covered,
            "真实 Roboto 缺 U600，不能把前段或 .notdef 当成候选成功"
        );
        worker.restart_font_probe(&mut probe).unwrap();
        // The missing-glyph Job has actually retired. Trial Amiri from the same
        // grapheme start without replacing Source or renewing an expired scope.
        let (covered, spans) =
            trial_actual_segments(&mut worker, &mut probe, &actual, &bidi.levels);
        assert!(covered, "完整固定 Amiri 必须实际覆盖这个字素的全部形状");
        assert_eq!(spans, [0..1, 1..2]);
        assert!(!probe.shaping_segment_ready().unwrap());
        assert!(matches!(
            probe.shaping_options(&style()),
            Err(MarkdownLineComposeError::InvalidRequest)
        ));
        worker
            .provide_style(
                pending
                    .clone()
                    .respond_registered(pending.source_range(), &registered, style())
                    .unwrap(),
            )
            .unwrap();
        // Actual response consumption revokes the old trial immediately. The file
        // remains the same unedited original mmap input, independently verified below.
        assert!(matches!(
            probe.validate(),
            Err(MarkdownLineComposeError::Poisoned)
        ));
        assert_eq!(
            std::fs::read(fixture._directory.path().join("真实分项.md")).unwrap(),
            text.as_bytes()
        );
        assert!(!fixture.document.is_dirty() && !fixture.document.can_undo());
        drop((probe, worker, registered, actual, fixture));
        assert!(
            resources.is_released() && resources.snapshot().unwrap().actual_resources_released()
        );
        assert!(source.payload_released());
        assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
    }
}
