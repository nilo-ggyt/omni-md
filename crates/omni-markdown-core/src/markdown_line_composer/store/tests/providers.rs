//! 已归还旧Job的同Store来源，必须仍沿原Measure last-fitting与二阶段交接，不强制item换行。
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
    for _ in 0..10000 {
        match coordinator
            .step(MarkdownLineComposeBudget::default())
            .unwrap()
            .answer
        {
            MarkdownLineItemAnswer::NeedStyle(request) => {
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
    let choice = selected(&mut measure, &mut store, &second_proof, &mut breaks);
    assert!(
        matches!(choice,MarkdownSoftLineMeasureChoice::MeasuredBreak(selected) if selected.source_utf16==(0..6) && selected.x_advance_26_6==first_facts.x_advance_26_6)
    );
    assert_eq!(store.facts(&first_proof).unwrap().facts.source_utf16, 0..6); // 后续行可回读旧项，不重造Job或吞掉末尾。
}

#[test]
fn immutable_forest_finds_many_real_items_without_a_resident_item_vector() {
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
        drop(item); // 每次只保一个Job，Proof也在这次循环归还，不积累N个Weak控制块。
        assert!(store.forest.iter().flatten().count() <= 64);
        assert!(observer.snapshot().mapped_bytes <= 16 * 1024);
    }
    for offset in (0..fixture.source.utf16_len()).rev() {
        let proof = store.item_at(offset).unwrap().unwrap().proof;
        let facts = store.facts(&proof).unwrap();
        assert_eq!(
            facts.facts.source_utf16,
            (offset / 2) * 2..(offset / 2 + 1) * 2
        );
        assert!(proof.job.is_none()); // 回读只按Store认证origin，不能反造已析构原Job的弱票。
    }
    assert!(store.item_at(fixture.source.utf16_len()).unwrap().is_none());
    store.close().unwrap();
    let state = observer.snapshot();
    assert!(!state.file_open && state.mapped_bytes == 0 && !state.cleanup_failed);
}

#[test]
fn stored_selected_permit_builds_mixed_script_lines_across_real_item_boundaries() {
    // 只在有限oracle中先写完三种脚本的真实item；每个原Job写入后立即Drop。
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
        loop {
            match selected(&mut measure, &mut store, &proof, &mut breaks) {
                MarkdownSoftLineMeasureChoice::NeedNextItem { source_utf16, .. } => {
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
    // 连字、组合字符、代理对和RTL均由真实shaper产生；oracle另建原Job索引，不复用Store目录。
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
            // 同一借用内顺序包含内部UTF16与重复查询；只有真实cluster末端才是下一ordinal。
            for offset in range.clone() {
                let expected = original.containing(&mut item.job, offset).unwrap();
                for _ in 0..2 {
                    assert_eq!(
                        GlyphProvider::cluster(&mut provider, &source, &font, offset).unwrap(),
                        expected
                    );
                }
            }
            // 先从末端跳回开头，再反向遍历，确保hint不能把任意坐标误当作下一条记录。
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
        drop(item); // 原Job与其独立cluster文件归还后，再以新借用读取同Store记录。
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
    drop(fixture);
    assert!(source_observer.payload_released() && font_observer.payload_released());
    assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
}

#[test]
fn stored_provider_cluster_hint_cannot_hide_cancellation_or_expired_font() {
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
                // hint已实际填入，再归还最后的注册字体；不能凭旧字形绕过原FontCatalog弱票。
                drop(fixture.font);
                assert!(font_observer.payload_released());
            } else {
                cancellation.cancel(); // 同坐标缓存命中也必须先撤销这次实际Measure读取。
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
        // 内容、范围与坐标都相同，真实来源仍不同；错误必须关闭原文件，不交付缓存字形。
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
        // 先实际借用一字节使原映射驻留，再经真实回收路径消费EIO注入；不是伪造Store返回值。
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
        cancellation.cancel(); // 原源首错必须仍优先于后来的取消、外方来源与已经填入的hint。
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
    assert!(backing.failed()); // 首错记录继续保留；实际munmap与FD释放是另一项义务。
    assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0);
}
