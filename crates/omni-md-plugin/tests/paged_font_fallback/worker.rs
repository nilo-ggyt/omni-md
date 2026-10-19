//! 真实packet/Job的可续推进和终态归还；有限合成GCB不进入产品正文构造路径。
use omni_markdown_core::{MarkdownShapingBudget, MarkdownShapingResourceObserver};
use omni_md_plugin::native_surface::paged_font_fallback::{
    NATIVE_MARKDOWN_FONT_TRIAL_GLYPHS, NativeMarkdownFontTrialBudget,
    NativeMarkdownFontTrialInputProgress, NativeMarkdownFontTrialResult,
    NativeMarkdownFontTrialStatus, NativeMarkdownFontTrialWorker,
};

use super::*;

#[path = "worker/emfile.rs"]
mod emfile;
pub(super) use emfile::verify as verify_emfile;

fn issued(
    fixture: &mut Fixture,
    font: &NativeMarkdownPagedFont,
    mtm: MainThreadMarker,
) -> (NativeMarkdownFontFallback, NativeMarkdownFontTrial) {
    fixture.prepare();
    let mut fallback = NativeMarkdownFontFallback::new(
        fixture.source.clone(),
        &fixture.authority,
        font,
        &font.core_font().authority(),
        request(
            0..fixture.source.utf16_len(),
            *b"Latn",
            MarkdownShapingDirection::LeftToRight,
            fixture.source.utf16_len(),
        ),
        mtm,
    )
    .unwrap();
    let NativeMarkdownFontFallbackStep::Trial(trial) = fallback
        .next_trial(
            font,
            &font.core_font().authority(),
            &mut fixture.graphemes,
            &mut fixture.scripts,
            &|| true,
        )
        .unwrap()
    else {
        panic!("原完整GCB应签发唯一Base packet")
    };
    (fallback, *trial) // 仅拆开原独占packet的存储盒；不复制正文、字体或试次身份。
}

fn zero() -> NativeMarkdownFontTrialBudget {
    NativeMarkdownFontTrialBudget {
        input: MarkdownShapingBudget {
            bytes: 0,
            scalars: 0,
        },
        glyphs: 0,
        shape_calls: 0,
    }
}

fn into_checking(worker: &mut NativeMarkdownFontTrialWorker) {
    while worker.status() == NativeMarkdownFontTrialStatus::Ingesting {
        worker.step(NativeMarkdownFontTrialBudget::default());
    }
    assert_eq!(
        worker.status(),
        NativeMarkdownFontTrialStatus::AwaitingShape
    );
    let step = worker.step(NativeMarkdownFontTrialBudget {
        shape_calls: usize::MAX,
        ..zero()
    });
    assert_eq!(step.shape_calls, 1);
    assert_eq!(step.status, NativeMarkdownFontTrialStatus::CheckingGlyphs);
    assert_eq!(step.glyphs, 0); // 完整shape之后还未检查任何glyph，不能提前选字体。
}

fn released(observer: &MarkdownShapingResourceObserver) {
    let snapshot = observer.snapshot();
    assert_eq!(snapshot.files_open, 0);
    assert_eq!(snapshot.mappings_open, 0);
    assert_eq!(snapshot.mapped_bytes, 0);
    assert_eq!(snapshot.work_bytes, 0); // 原小输入可合法只占工作块，真实Drop仍须交还其实际分配。
    assert_eq!(snapshot.files_created, snapshot.files_closed);
    assert_eq!(snapshot.mappings_created, snapshot.mappings_released);
    assert!(!snapshot.cleanup_failed);
}

fn selected(
    fallback: &mut NativeMarkdownFontFallback,
    fixture: &mut Fixture,
    font: &NativeMarkdownPagedFont,
    result: NativeMarkdownFontTrialResult,
) -> omni_md_plugin::native_surface::paged_font_fallback::NativeMarkdownFontGlyphFacts {
    stores_released(&result);
    let NativeMarkdownFontFallbackCompletion::Selected(choice) = fallback
        .complete_trial(
            font,
            &font.core_font().authority(),
            &mut fixture.graphemes,
            &mut fixture.scripts,
            result,
            &|| true,
        )
        .unwrap()
    else {
        panic!("原Helvetica应完整支持这个Latin组合GCB")
    };
    assert_eq!(choice.source_range(), 0..fixture.source.utf16_len());
    assert_eq!(choice.origin(), NativeMarkdownFontOrigin::Base);
    assert_eq!(choice.facts().missing_glyphs, 0);
    choice.facts()
}

fn budgeted_worker_matches_compatibility_driver(mtm: MainThreadMarker) {
    let text = format!("a{}", "\u{0301}".repeat(600));
    let mut fixture = Fixture::new(&text);
    let font = base(mtm);
    let (mut original, trial) = issued(&mut fixture, &font, mtm);
    let (mut continued, packet) = issued(&mut fixture, &font, mtm);
    let expected_bytes = text.len();
    let expected_scalars = text.chars().count();
    let result = std::thread::spawn(move || {
        let mut worker = packet.into_worker();
        assert_eq!(worker.status(), NativeMarkdownFontTrialStatus::Ingesting);
        let observer = worker.resource_observer().unwrap();
        // 任一输入维度为0都不调用Core的非法零预算接口；同Job保持可继续，未伪报成功。
        for input in [
            MarkdownShapingBudget {
                bytes: 0,
                scalars: 1,
            },
            MarkdownShapingBudget {
                bytes: 1,
                scalars: 0,
            },
        ] {
            let step = worker.step(NativeMarkdownFontTrialBudget { input, ..zero() });
            assert_eq!(
                step.input,
                NativeMarkdownFontTrialInputProgress::NotAttempted
            );
            assert_eq!(step.status, NativeMarkdownFontTrialStatus::Ingesting);
        }
        let (mut bytes, mut scalars, mut incomplete_utf8) = (0, 0, false);
        while worker.status() == NativeMarkdownFontTrialStatus::Ingesting {
            let step = worker.step(NativeMarkdownFontTrialBudget {
                input: MarkdownShapingBudget {
                    bytes: 1,
                    scalars: 1,
                },
                ..zero()
            });
            let NativeMarkdownFontTrialInputProgress::Counted {
                bytes: used,
                scalars: added,
            } = step.input
            else {
                panic!("真实1-byte输入步失败或丢失计费")
            };
            assert!(used <= 1 && added <= 1);
            assert_eq!(step.shape_calls, 0);
            assert_eq!(step.glyphs, 0);
            bytes += used;
            scalars += added;
            incomplete_utf8 |= used == 1 && added == 0;
        }
        assert!(incomplete_utf8);
        assert_eq!((bytes, scalars), (expected_bytes, expected_scalars));
        assert_eq!(
            worker.status(),
            NativeMarkdownFontTrialStatus::AwaitingShape
        );
        let waiting = worker.step(zero());
        assert_eq!(waiting.shape_calls, 0);
        assert_eq!(waiting.status, NativeMarkdownFontTrialStatus::AwaitingShape);
        let shaped = worker.step(NativeMarkdownFontTrialBudget {
            shape_calls: usize::MAX,
            ..zero()
        });
        assert_eq!(shaped.shape_calls, 1);
        assert_eq!(shaped.glyphs, 0);
        assert_eq!(
            worker.status(),
            NativeMarkdownFontTrialStatus::CheckingGlyphs
        );
        assert_eq!(worker.step(zero()).glyphs, 0);
        let (mut glyphs, mut batches) = (0, 0);
        while worker.status() != NativeMarkdownFontTrialStatus::Finished {
            let limit = if batches == 0 { 1 } else { usize::MAX };
            let step = worker.step(NativeMarkdownFontTrialBudget {
                glyphs: limit,
                ..zero()
            });
            assert!(step.glyphs <= limit.min(NATIVE_MARKDOWN_FONT_TRIAL_GLYPHS));
            assert_eq!(
                step.input,
                NativeMarkdownFontTrialInputProgress::NotAttempted
            );
            assert_eq!(step.shape_calls, 0);
            glyphs += step.glyphs;
            batches += 1;
        }
        assert!(glyphs > NATIVE_MARKDOWN_FONT_TRIAL_GLYPHS && batches >= 3);
        released(&observer);
        let ended = worker.step(NativeMarkdownFontTrialBudget::default());
        assert_eq!(ended.glyphs, 0);
        assert_eq!(ended.shape_calls, 0); // 重复观察终态不能重复shape或累计同glyph。
        let result = worker.into_result();
        assert_eq!(result.outcome().unwrap().glyph_count, glyphs);
        result
    })
    .join()
    .unwrap();
    let reference = std::thread::spawn(move || trial.run()).join().unwrap();
    let expected = selected(&mut original, &mut fixture, &font, reference);
    let actual = selected(&mut continued, &mut fixture, &font, result);
    assert_eq!(
        (
            actual.glyph_count,
            actual.missing_glyphs,
            actual.flags_union,
            actual.input_scalars,
            actual.source_bytes_read
        ),
        (
            expected.glyph_count,
            expected.missing_glyphs,
            expected.flags_union,
            expected.input_scalars,
            expected.source_bytes_read
        )
    );
    assert_eq!(actual.source_borrow_max_bytes, 1);
    assert!(expected.source_borrow_max_bytes <= 4096); // 计费来自不同真实量子，不要求两者借用峰值伪相等。
    assert_eq!(actual.input_scalars, expected_scalars);
    println!(
        "paged_font_trial resumable=true input_byte_budget=1 scalar_budget=1 glyph_budget_max=256 shapes=1 exact_compatibility_glyph_facts=true glyphs={} indivisible_gcb=true",
        actual.glyph_count
    );
}

fn early_result_and_drop_return_actual_stores(mtm: MainThreadMarker) {
    let text = format!("a{}", "\u{0301}".repeat(300));
    let mut fixture = Fixture::new(&text);
    let font = base(mtm);
    for phase in 0..3 {
        let (mut fallback, packet) = issued(&mut fixture, &font, mtm);
        let result = std::thread::spawn(move || {
            let mut worker = packet.into_worker();
            let observer = worker.resource_observer().unwrap();
            if phase == 0 {
                worker.step(NativeMarkdownFontTrialBudget {
                    input: MarkdownShapingBudget {
                        bytes: 1,
                        scalars: 1,
                    },
                    ..zero()
                });
            } else {
                into_checking(&mut worker);
                worker.step(NativeMarkdownFontTrialBudget {
                    glyphs: 1,
                    ..zero()
                });
            }
            // 此控制验证三阶段早退，不要求短GCB为测试额外开FD；先证原Job确实占有工作块，
            // 再由下方同observer同时核工作块归零及所有实际创建的FD/map成对归还。
            assert!(observer.snapshot().work_bytes > 0);
            let result = if phase == 2 {
                let result = worker.into_result(); // 未检查完整输出只能得到取消，不能取得前缀成功facts。
                assert!(matches!(
                    result.outcome(),
                    Err(NativeMarkdownFontFallbackError::Shaping(
                        MarkdownShapingError::Cancelled
                    ))
                ));
                Some(result)
            } else {
                drop(worker); // 两个真实阶段的早退都必须在返回前归还原工作块及已有FD/map。
                None
            };
            released(&observer);
            assert!(observer.snapshot().first_error.is_none());
            result
        })
        .join()
        .unwrap();
        if let Some(result) = result {
            assert!(matches!(
                fallback.complete_trial(
                    &font,
                    &font.core_font().authority(),
                    &mut fixture.graphemes,
                    &mut fixture.scripts,
                    result,
                    &|| true
                ),
                Err(NativeMarkdownFontFallbackError::Shaping(
                    MarkdownShapingError::Cancelled
                ))
            ));
        }
        fallback.close();
    }
    println!(
        "paged_font_trial input_drop=true glyph_drop=true early_result_cancelled=true actual_stores_released=true"
    );
}

// Check the actual captured Cascade bytes on completed-error teardown and on a foreign result.
fn completed_cascade_error_returns_original_bytes(mtm: MainThreadMarker) {
    for foreign in [false, true] {
        let mut fixture = Fixture::new("中");
        fixture.prepare();
        let source = fixture.source.resource_observer();
        let mapping = fixture.document.original_backing_observer().unwrap();
        let font = base(mtm);
        let authority = font.core_font().authority();
        let mut fallback = NativeMarkdownFontFallback::new(
            fixture.source.clone(),
            &fixture.authority,
            &font,
            &authority,
            request(
                0..fixture.source.utf16_len(),
                *b"Hani",
                MarkdownShapingDirection::LeftToRight,
                fixture.source.utf16_len(),
            ),
            mtm,
        )
        .unwrap();
        let NativeMarkdownFontFallbackStep::Trial(base_trial) = fallback
            .next_trial(
                &font,
                &authority,
                &mut fixture.graphemes,
                &mut fixture.scripts,
                &|| true,
            )
            .unwrap()
        else {
            panic!("The prepared original GCB must issue the Base trial first")
        };
        assert!(fallback.active_font_bytes_observer().is_none());
        let result = std::thread::spawn(move || base_trial.run()).join().unwrap();
        stores_released(&result);
        // A real missing-glyph result advances the original cascade; the test does not invent an origin.
        assert!(matches!(
            fallback.complete_trial(
                &font,
                &authority,
                &mut fixture.graphemes,
                &mut fixture.scripts,
                result,
                &|| true,
            ),
            Ok(NativeMarkdownFontFallbackCompletion::Rejected {
                origin: NativeMarkdownFontOrigin::Base,
                facts,
            }) if facts.missing_glyphs > 0
        ));
        let mut captured = None;
        for _ in 0..omni_md_plugin::native_surface::paged_font_cascade::NATIVE_MARKDOWN_CASCADE_MAX_CANDIDATES {
            match fallback.next_trial(
                &font,
                &authority,
                &mut fixture.graphemes,
                &mut fixture.scripts,
                &|| true,
            ).unwrap() {
                NativeMarkdownFontFallbackStep::Trial(packet) => {
                    captured = Some(*packet);
                    break;
                }
                NativeMarkdownFontFallbackStep::UnavailableCandidate { .. } => {}
                _ => panic!("The original system cascade must contain a capturable candidate"),
            }
        }
        let packet = captured.expect("A real Cascade packet is required");
        let bytes = fallback
            .active_font_bytes_observer()
            .expect("The Cascade capture must expose its original fixed resource observer");
        assert!(bytes.captured_bytes() > 0 && bytes.mapped_bytes() > 0);
        assert!(!bytes.mapping_drop_completed());
        if foreign {
            let mut other_fixture = Fixture::new("a");
            let other_source = other_fixture.source.resource_observer();
            let other_mapping = other_fixture.document.original_backing_observer().unwrap();
            let (other_fallback, other_packet) = issued(&mut other_fixture, &font, mtm);
            let result = std::thread::spawn(move || other_packet.run())
                .join()
                .unwrap();
            stores_released(&result);
            let cause = fallback
                .complete_trial(
                    &font,
                    &authority,
                    &mut fixture.graphemes,
                    &mut fixture.scripts,
                    result,
                    &|| true,
                )
                .err()
                .expect("A foreign nonce must not complete the original capture");
            assert!(matches!(
                cause,
                NativeMarkdownFontFallbackError::TrialMismatch
            ));
            // The original, unconsumed packet still legally owns the Core font bytes. Rejecting a
            // foreign result must not claim that this separate outstanding owner has released them.
            assert!(!bytes.mapping_drop_completed() && bytes.mapped_bytes() > 0);
            drop(packet);
            other_fallback.close();
            drop(other_fixture);
            assert!(other_source.payload_released() && other_mapping.unmap_succeeded());
            assert_eq!(other_mapping.mapped_bytes(), 0);
            assert!(!other_mapping.failed());
        } else {
            let result = std::thread::spawn(move || {
                let mut worker = packet.into_worker();
                let observer = worker.resource_observer().unwrap();
                worker.step(NativeMarkdownFontTrialBudget {
                    input: MarkdownShapingBudget {
                        bytes: 1,
                        scalars: 1,
                    },
                    ..zero()
                });
                worker.cancellation().request();
                let ended = worker.step(zero());
                assert_eq!(ended.status, NativeMarkdownFontTrialStatus::Finished);
                let result = worker.into_result();
                assert!(matches!(
                    result.outcome(),
                    Err(NativeMarkdownFontFallbackError::Shaping(
                        MarkdownShapingError::Cancelled
                    ))
                ));
                released(&observer);
                result
            })
            .join()
            .unwrap();
            assert!(!bytes.mapping_drop_completed());
            let cause = fallback
                .complete_trial(
                    &font,
                    &authority,
                    &mut fixture.graphemes,
                    &mut fixture.scripts,
                    result,
                    &|| true,
                )
                .err()
                .expect("A completed cancelled trial must remain a failure");
            assert!(matches!(
                cause,
                NativeMarkdownFontFallbackError::Shaping(MarkdownShapingError::Cancelled)
            ));
        }
        // Observe the same capture before dropping the fallback. Its real completion-error path,
        // or the last legitimate packet above, must already have returned both descriptors and VM.
        assert!(bytes.mapping_drop_completed() && bytes.unmap_succeeded());
        assert_eq!(bytes.mapped_bytes(), 0);
        assert!(bytes.source_descriptor_closed() && bytes.backing_descriptor_closed());
        assert!(!bytes.failed());
        fallback.close();
        drop(fixture);
        assert!(source.payload_released() && mapping.unmap_succeeded() && !mapping.failed());
        assert_eq!(mapping.mapped_bytes(), 0);
        let base_bytes = font.bytes_observer();
        drop(font);
        assert!(base_bytes.mapping_drop_completed() && base_bytes.unmap_succeeded());
        assert_eq!(base_bytes.mapped_bytes(), 0);
        assert!(base_bytes.source_descriptor_closed() && base_bytes.backing_descriptor_closed());
        assert!(!base_bytes.failed());
        println!(
            "paged_font_trial completed_cascade_error=true foreign_result={foreign} actual_captured_font_bytes_returned=true exact_error_preserved=true actual_source_unmapped=true formal_memory=false"
        );
    }
}

fn original_close_is_observed_between_batches(mtm: MainThreadMarker) {
    let text = format!("a{}", "\u{0301}".repeat(300));
    let mut fixture = Fixture::new(&text);
    let font = base(mtm);
    let (fallback, packet) = issued(&mut fixture, &font, mtm);
    let source = fixture.source.resource_observer();
    let mapping = fixture.document.original_backing_observer().unwrap();
    let gate = Arc::new(Barrier::new(2));
    let worker_gate = gate.clone();
    let thread = std::thread::spawn(move || {
        let mut worker = packet.into_worker();
        let observer = worker.resource_observer().unwrap();
        into_checking(&mut worker);
        assert_eq!(
            worker
                .step(NativeMarkdownFontTrialBudget {
                    glyphs: 1,
                    ..zero()
                })
                .glyphs,
            1
        );
        worker_gate.wait();
        worker_gate.wait(); // 主线程此时已真正关闭原fallback并交还Doc/Source，不靠sleep猜时间。
        let step = worker.step(zero());
        assert_eq!(step.status, NativeMarkdownFontTrialStatus::Finished);
        assert_eq!(step.glyphs, 0);
        let result = worker.into_result();
        assert!(matches!(
            result.outcome(),
            Err(NativeMarkdownFontFallbackError::Shaping(
                MarkdownShapingError::Cancelled
            ))
        ));
        released(&observer);
        result
    });
    gate.wait();
    fallback.close();
    drop(fixture);
    assert!(!source.payload_released() && !mapping.unmap_succeeded());
    gate.wait();
    let result = thread.join().unwrap();
    stores_released(&result);
    assert!(source.payload_released() && mapping.unmap_succeeded() && !mapping.failed());
    assert_eq!(mapping.mapped_bytes(), 0);
    println!(
        "paged_font_trial original_cancel_between_batches=true zero_budget_observes_cancel=true source_actual_unmapped=true"
    );
}

fn main_thread_construction_does_not_shape(mtm: MainThreadMarker) {
    let mut fixture = Fixture::new("a");
    let font = base(mtm);
    let (fallback, packet) = issued(&mut fixture, &font, mtm);
    let worker = packet.into_worker();
    assert_eq!(worker.status(), NativeMarkdownFontTrialStatus::Finished);
    assert!(worker.resource_observer().is_none());
    let result = worker.into_result();
    assert!(matches!(
        result.outcome(),
        Err(NativeMarkdownFontFallbackError::MainThreadShape)
    ));
    assert!(result.resources().is_none());
    fallback.close();
    println!("paged_font_trial main_thread_refused=true no_fake_resource_receipt=true");
}

pub(super) fn verify(mtm: MainThreadMarker) {
    budgeted_worker_matches_compatibility_driver(mtm);
    early_result_and_drop_return_actual_stores(mtm);
    completed_cascade_error_returns_original_bytes(mtm);
    original_close_is_observed_between_batches(mtm);
    main_thread_construction_does_not_shape(mtm);
}
