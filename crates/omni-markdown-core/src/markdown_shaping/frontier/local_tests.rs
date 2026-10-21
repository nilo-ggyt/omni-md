//! Compare every glyph with a finite complete oracle whose independent job uses no local
//! certificate or center view.
use super::*;

fn compare_part(fixture: &Fixture, child: &mut MarkdownShapingJob, full: &mut MarkdownShapingJob) {
    let range = child.source_range();
    let mut ordinal = 0;
    for index in 0..full.profile().unwrap().glyph_count {
        let glyph = full
            .glyph(&fixture.authority, &fixture.font_authority, index)
            .unwrap()
            .unwrap();
        if range.contains(&glyph.source_utf16_cluster) {
            assert_eq!(
                child
                    .glyph(&fixture.authority, &fixture.font_authority, ordinal)
                    .unwrap(),
                Some(glyph),
                "中心{range:?}第{ordinal}条与独立完整任务不同"
            );
            ordinal += 1;
        }
    }
    assert_eq!(child.profile().unwrap().glyph_count, ordinal);
    assert!(
        child
            .glyph(&fixture.authority, &fixture.font_authority, ordinal)
            .unwrap()
            .is_none()
    );
}

#[test]
#[ignore = "显式使用原Native UIFont抓取的有限字体输入；须设置OMNI_FRONTIER_UIFONT_FIXTURES"]
fn actual_native_ui_fonts_local_centers_match_full_source_glyphs_flags_and_advances() {
    // These 18 name-table/axis/point-size cases come from the original Native capture, without
    // guessing faces from names or substituting monospace. Read fixtures only: missing inputs fail
    // explicitly, and an unexecuted role is never a zero-difference acceptance result.
    let root = std::path::PathBuf::from(
        std::env::var_os("OMNI_FRONTIER_UIFONT_FIXTURES").expect("需要真实UIFont控制输入"),
    );
    let rows: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("fonts.json")).unwrap()).unwrap();
    let mut tested = 0;
    for row in rows.as_array().unwrap() {
        let label = row["label"].as_str().unwrap();
        let axes: Vec<_> = row["axes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|axis| crate::MarkdownLayoutFontAxis {
                tag: (axis["tag"].as_u64().unwrap() as u32).to_be_bytes(),
                value: axis["value"].as_f64().unwrap() as f32,
            })
            .collect();
        let font = MarkdownLayoutFont::resolve_name_table_at_size(
            Arc::new(std::fs::read(row["path"].as_str().unwrap()).unwrap()),
            &std::fs::read(root.join(row["name_file"].as_str().unwrap())).unwrap(),
            &axes,
            row["points"].as_f64().unwrap(),
        )
        .unwrap();
        let text = format!(
            "{}{}",
            "before offset ",
            "AV Ta To Wa fi ffi ij i:j : : x:y 10/123 !? []() {} ".repeat(100)
        );
        let mut fixture = Fixture::new(&text, font);
        let range = 14..text.len();
        let mut options = fixture.options();
        options.beginning_of_text = false;
        let mut frontier = MarkdownShapingFrontierJob::new(
            fixture.source.clone(),
            &fixture.authority,
            fixture.font.clone(),
            &fixture.font_authority,
            range.clone(),
            options.clone(),
            MarkdownShapingCancellation::default(),
        )
        .unwrap();
        let mut full = MarkdownShapingJob::new(
            fixture.source.clone(),
            &fixture.authority,
            fixture.font.clone(),
            &fixture.font_authority,
            range.clone(),
            options,
        )
        .unwrap();
        while full.status() == MarkdownShapingStatus::Ingesting {
            full.step(
                &fixture.authority,
                &fixture.font_authority,
                MarkdownShapingBudget::default(),
            )
            .unwrap();
        }
        full.shape(&fixture.authority, &fixture.font_authority)
            .unwrap();
        // The actual item starts at a nonzero source coordinate; inherit its original boundary
        // options rather than treating it as the source beginning.
        let mut parts = 0;
        let mut metadata = 0;
        let mut input = 0;
        let mut context = 0;
        let mut covered = range.start;
        loop {
            let mut ready = false;
            for _ in 0..300_000 {
                fixture
                    .breaks
                    .step(
                        &fixture.authority,
                        MarkdownLineBreakBudget {
                            bytes: 1024,
                            scalars: 1024,
                            records: 1024,
                        },
                    )
                    .unwrap(); // Full-glyph comparison is not first-line latency; advance actual GCB dependencies instead of declaring Pending EOF.
                let step = frontier
                    .step(
                        &fixture.authority,
                        &fixture.font_authority,
                        &mut fixture.breaks,
                        MarkdownShapingBudget {
                            bytes: 37,
                            scalars: 37,
                        },
                        1,
                    )
                    .unwrap();
                assert!(step.plan_records + step.metadata_glyphs <= 1);
                assert!(step.source_bytes + step.input_bytes <= 37);
                metadata += step.plan_records;
                input += step.input_bytes;
                context += step.context_bytes;
                match step.answer {
                    MarkdownShapingFrontierAnswer::Pending => {}
                    MarkdownShapingFrontierAnswer::PartReady => {
                        ready = true;
                        break;
                    }
                    other => panic!("实际角色{label}尚未认证: {other:?}; 已读metadata={metadata}"),
                }
            }
            assert!(
                ready,
                "真实记录游标没有完成: role={label} next={} metadata={metadata} support={:?}",
                frontier.next, frontier.local_capability
            );
            let (mut child, continuation) = frontier
                .take_part(&fixture.authority, &fixture.font_authority)
                .unwrap()
                .into_parts();
            assert_eq!(child.source_range().start, covered);
            compare_part(&fixture, &mut child, &mut full);
            covered = child.source_range().end;
            parts += 1;
            let observer = child.resource_observer();
            drop(child);
            assert_eq!(observer.snapshot().files_open, 0);
            assert_eq!(observer.snapshot().mappings_open, 0);
            frontier
                .resume(&fixture.authority, &fixture.font_authority, continuation)
                .unwrap();
            if covered == range.end {
                break;
            }
        }
        eprintln!(
            "actual_ui_local role={label} parts={parts} support={:?} metadata={metadata} input={input} context={context}",
            frontier.local_capability
        );
        assert_eq!(covered, range.end);
        tested += 1;
    }
    assert_eq!(tested, 18);
}

#[test]
fn original_roboto_complete_ascii_domain_centers_match_full_real_plan() {
    let font = MarkdownLayoutFont::resolve(Arc::new(ROBOTO), "Roboto-Regular", &[]).unwrap();
    let text = format!("中文\r\nX{}", "a".repeat(1023));
    let mut fixture = Fixture::new(&text, font);
    let range = 4..fixture.source.utf16_len();
    let mut options = fixture.options();
    options.beginning_of_text = false;
    let mut frontier = MarkdownShapingFrontierJob::new(
        fixture.source.clone(),
        &fixture.authority,
        fixture.font.clone(),
        &fixture.font_authority,
        range.clone(),
        options.clone(),
        MarkdownShapingCancellation::default(),
    )
    .unwrap();
    let mut full = MarkdownShapingJob::new(
        fixture.source.clone(),
        &fixture.authority,
        fixture.font.clone(),
        &fixture.font_authority,
        range.clone(),
        options,
    )
    .unwrap();
    while full.status() == MarkdownShapingStatus::Ingesting {
        full.step(
            &fixture.authority,
            &fixture.font_authority,
            MarkdownShapingBudget {
                bytes: 37,
                scalars: 37,
            },
        )
        .unwrap();
    }
    full.shape(&fixture.authority, &fixture.font_authority)
        .unwrap();
    let mut covered = range.start;
    while covered != range.end {
        let mut ready = false;
        for _ in 0..300_000 {
            let step = frontier
                .step(
                    &fixture.authority,
                    &fixture.font_authority,
                    &mut fixture.breaks,
                    MarkdownShapingBudget {
                        bytes: 1,
                        scalars: 1,
                    },
                    1,
                )
                .unwrap();
            assert!(step.source_bytes + step.input_bytes <= 1 && step.scalars <= 1);
            assert!(step.plan_records + step.metadata_glyphs <= 1);
            match step.answer {
                MarkdownShapingFrontierAnswer::Pending => {}
                MarkdownShapingFrontierAnswer::PartReady => {
                    ready = true;
                    break;
                }
                other => panic!("原Roboto完整Xa域仍未取得真实能力：{other:?}"),
            }
        }
        assert!(ready);
        assert_eq!(frontier.local_source_bounds, Some((b'X', b'a')));
        let (mut child, continuation) = frontier
            .take_part(&fixture.authority, &fixture.font_authority)
            .unwrap()
            .into_parts();
        assert_eq!(child.source_range().start, covered);
        compare_part(&fixture, &mut child, &mut full); // Compare IDs/flags/clusters/advances/offsets, not only counts.
        covered = child.source_range().end;
        let resources = child.resource_observer();
        drop(child);
        assert_eq!(resources.snapshot().files_open, 0);
        assert_eq!(resources.snapshot().mappings_open, 0);
        frontier
            .resume(&fixture.authority, &fixture.font_authority, continuation)
            .unwrap();
    }
}

#[test]
fn reachable_roboto_fi_and_unread_late_fi_keep_the_real_ligature_refusal() {
    for text in ["fi".repeat(300), format!("X{}fi", "a".repeat(8192))] {
        let font = MarkdownLayoutFont::resolve(Arc::new(ROBOTO), "Roboto-Regular", &[]).unwrap();
        let mut fixture = Fixture::new(&text, font);
        let mut frontier = fixture.frontier(0..fixture.source.utf16_len());
        let mut rejection = None;
        for _ in 0..300_000 {
            let step = frontier
                .step(
                    &fixture.authority,
                    &fixture.font_authority,
                    &mut fixture.breaks,
                    MarkdownShapingBudget {
                        bytes: 1,
                        scalars: 1,
                    },
                    1,
                )
                .unwrap();
            assert!(step.source_bytes + step.input_bytes <= 1 && step.scalars <= 1);
            assert!(step.plan_records + step.metadata_glyphs <= 1);
            match step.answer {
                MarkdownShapingFrontierAnswer::Pending => {}
                MarkdownShapingFrontierAnswer::NeedsFrontier(reason) => {
                    rejection = Some(reason);
                    break;
                }
                other => panic!("未读尾部真实可达liga不能被当前Xa前缀冒充安全：{other:?}"),
            }
        }
        assert_eq!(
            rejection,
            Some(MarkdownShapingFrontierReason::Plan(
                harfrust::AsciiFrontierRejection::CrossInputSubtable
            ))
        );
        assert!(frontier.child.is_none()); // Reject before glyph creation without disabling original full shaping or features.
    }
}

#[test]
fn pending_complete_domain_proof_keeps_original_cancel_and_eio_cause_without_live_paths() {
    // Pause a real complete-domain tree walk, then cancel with or without actual source reclaim
    // failure. Ending the frontier must return its unfinished tree path and actual source owner.
    for failed_io in [false, true] {
        let font = MarkdownLayoutFont::resolve(Arc::new(ROBOTO), "Roboto-Regular", &[]).unwrap();
        let mut fixture = Fixture::new(&format!("中文\r\nX{}", "a".repeat(1023)), font);
        let mapping = fixture.document.original_backing_observer().unwrap();
        let payload = fixture.source.resource_observer();
        let mut frontier = fixture.frontier(4..fixture.source.utf16_len());
        let progress = frontier
            .step(
                &fixture.authority,
                &fixture.font_authority,
                &mut fixture.breaks,
                MarkdownShapingBudget {
                    bytes: 1,
                    scalars: 1,
                },
                1,
            )
            .unwrap();
        assert_eq!(progress.answer, MarkdownShapingFrontierAnswer::Pending);
        assert!(frontier.local_source_cursor.is_some());
        if failed_io {
            fixture.source.visit_byte_range(0..1, |_| Ok(())).unwrap();
            fixture.document.fail_next_original_reclaim_for_test();
            assert!(
                fixture
                    .document
                    .reclaim_original_read_windows_for_test()
                    .is_err()
            );
        }
        frontier.cancellation.request();
        let error = frontier
            .step(
                &fixture.authority,
                &fixture.font_authority,
                &mut fixture.breaks,
                MarkdownShapingBudget {
                    bytes: 1,
                    scalars: 1,
                },
                1,
            )
            .err()
            .unwrap();
        if failed_io {
            assert!(matches!(
                error,
                MarkdownShapingFrontierError::Shaping(MarkdownShapingError::Source(
                    crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
                ))
            ));
        } else {
            assert!(matches!(
                error,
                MarkdownShapingFrontierError::Shaping(MarkdownShapingError::Cancelled)
            ));
        }
        assert!(frontier.local_source_cursor.is_none() && frontier.payload.is_none());
        drop((fixture, frontier));
        assert!(
            payload.payload_released() && mapping.unmap_succeeded() && mapping.mapped_bytes() == 0
        );
    }
}
