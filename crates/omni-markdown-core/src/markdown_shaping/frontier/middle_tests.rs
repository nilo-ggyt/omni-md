//! 中部真实Font交接沿原SCX续排；以完整字体作业逐字段对照，不删除emoji或伪造边界。
use super::*;

fn emoji_font() -> (
    MarkdownLayoutFont,
    crate::MarkdownLayoutFontBytesResourceObserver,
) {
    let bytes = crate::MarkdownLayoutFontBytes::capture_file(
        std::path::Path::new("/System/Library/Fonts/Apple Color Emoji.ttc"),
        512 * 1024 * 1024,
    )
    .unwrap();
    let observer = bytes.resource_observer();
    let name = read_fonts::FontRef::from_index(bytes.as_ref(), 0)
        .unwrap()
        .table_data(read_fonts::types::Tag::new(b"name"))
        .unwrap()
        .as_bytes()
        .to_vec();
    let font =
        MarkdownLayoutFont::resolve_name_table_at_size(bytes.blob(), &name, &[], 16.0).unwrap();
    (font, observer)
}

#[test]
fn middle_pair_and_later_emoji_keep_real_runs_original_plan_and_exactly_once_cursor() {
    // 两个相邻emoji与后继é仍处于同一自然Latn SCX；真正Han与CRLF封闭原项。
    let text = format!(
        "{}😀😀{}😀{}中\r\n",
        "é".repeat(462),
        "é".repeat(300),
        "é".repeat(10)
    );
    let fixture = Fixture::new(&text, independent_font());
    let (emoji, mapped) = emoji_font();
    let source = fixture.source.resource_observer();
    let backing = fixture.document.original_backing_observer().unwrap();
    let original_font = fixture.font.resource_observer();
    let mut worker = MarkdownLineItemCoordinator::new_with_frontier(
        fixture.source.clone(),
        &fixture.authority,
        MarkdownLineComposeCancellation::new(),
    )
    .unwrap();
    let mut records = Vec::new();
    let mut requests = Vec::new();
    let mut held_once = false;
    for _ in 0..100_000 {
        match advance(&mut worker) {
            MarkdownLineItemAnswer::Yielded => {}
            MarkdownLineItemAnswer::NeedStyle(request) => {
                let range = request.source_range();
                requests.push(range.clone());
                let font = if request.continuation_font().is_some() {
                    let probe = worker.font_probe(&request).unwrap();
                    let original = trial(&fixture, &probe, &fixture.font);
                    if original.iter().any(|glyph| glyph.glyph_id == 0) {
                        let fallback = trial(&fixture, &probe, &emoji);
                        assert!(
                            !fallback.is_empty()
                                && fallback.iter().all(|glyph| glyph.glyph_id != 0)
                        );
                        emoji.clone()
                    } else {
                        fixture.font.clone()
                    }
                } else {
                    fixture.font.clone()
                };
                worker
                    .provide_style(request.respond(range, font, style()).unwrap())
                    .unwrap();
            }
            MarkdownLineItemAnswer::ItemReady(_) => {
                let item = worker.take_item().unwrap();
                if item.facts().source_utf16 == (462..466) {
                    // Store未归还的实际child阻塞原游标；不能把Ready头部或Weak副本当成完成。
                    assert!(!held_once);
                    held_once = true;
                    assert!(matches!(
                        advance(&mut worker),
                        MarkdownLineItemAnswer::NeedsFrontier(
                            crate::MarkdownLineFrontierReason::PendingConsumer
                        )
                    ));
                }
                let record = glyphs(item);
                let end = record.0.source_utf16.end;
                records.push(record);
                if end == 778 {
                    break;
                }
            }
            MarkdownLineItemAnswer::NeedsFrontier(reason) => {
                panic!("真实中部Font交接仍被拒绝: {reason:?}")
            }
            _ => panic!("真实中部Font交接不能丢失正文或提前EOF"),
        }
    }
    assert!(held_once);
    assert_eq!(
        requests,
        vec![0..778, 462..464, 464..466, 466..467, 766..768, 768..769]
    );
    let ranges = records
        .iter()
        .map(|record| record.0.source_utf16.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        ranges,
        vec![
            0..256,
            256..462,
            462..466,
            466..722,
            722..766,
            766..768,
            768..778
        ]
    );
    assert_eq!(
        records[1].1.len(),
        206,
        "原Font的206字素残余必须先于新Font完整交付"
    );
    for (facts, actual) in &records {
        let font = if facts.source_utf16 == (462..466) || facts.source_utf16 == (766..768) {
            &emoji
        } else {
            &fixture.font
        };
        assert_eq!(*actual, oracle(&fixture, font, facts.source_utf16.clone()));
        assert!(!facts.end_of_text, "Font边界不能冒充真实段落末端");
    }
    drop((worker, fixture, emoji));
    assert!(source.payload_released() && original_font.payload_released());
    assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
    assert!(
        mapped.mapping_drop_completed()
            && mapped.unmap_succeeded()
            && mapped.mapped_bytes() == 0
            && !mapped.failed()
    );
}

#[test]
fn middle_actual_third_font_seed_closes_two_runs_without_replaying_the_gcb_or_losing_suffix() {
    let fixture = Fixture::new(
        &format!("{}😀😀♜♜{}中\r\n", "é".repeat(462), "é".repeat(10)),
        independent_font(),
    );
    let (emoji, emoji_mapped) = emoji_font();
    let bytes = crate::MarkdownLayoutFontBytes::capture_file(
        std::path::Path::new("/System/Library/Fonts/Apple Symbols.ttf"),
        128 * 1024 * 1024,
    )
    .unwrap();
    let symbols_mapped = bytes.resource_observer();
    let name = read_fonts::FontRef::from_index(bytes.as_ref(), 0)
        .unwrap()
        .table_data(read_fonts::types::Tag::new(b"name"))
        .unwrap()
        .as_bytes()
        .to_vec();
    let symbols =
        MarkdownLayoutFont::resolve_name_table_at_size(bytes.blob(), &name, &[], 16.0).unwrap();
    drop(bytes);
    let source = fixture.source.resource_observer();
    let backing = fixture.document.original_backing_observer().unwrap();
    let mut worker = MarkdownLineItemCoordinator::new_with_frontier(
        fixture.source.clone(),
        &fixture.authority,
        MarkdownLineComposeCancellation::new(),
    )
    .unwrap();
    let mut requests = Vec::new();
    let mut records = Vec::new();
    for _ in 0..100_000 {
        match advance(&mut worker) {
            MarkdownLineItemAnswer::Yielded => {}
            MarkdownLineItemAnswer::NeedStyle(request) => {
                let range = request.source_range();
                requests.push(range.clone());
                let selected = if request.continuation_font().is_some() {
                    let probe = worker.font_probe(&request).unwrap();
                    let mut selected = None;
                    // 真正完整字形试次按原字体、当前回退、第三字体顺序选择；不靠cmap猜缺字。
                    for font in [&fixture.font, &emoji, &symbols] {
                        let actual = trial(&fixture, &probe, font);
                        if !actual.is_empty() && actual.iter().all(|glyph| glyph.glyph_id != 0) {
                            selected = Some(font.clone());
                            break;
                        }
                    }
                    selected.expect("三个真实字体必须实际覆盖这个合成GCB")
                } else {
                    fixture.font.clone()
                };
                worker
                    .provide_style(request.respond(range, selected, style()).unwrap())
                    .unwrap();
            }
            MarkdownLineItemAnswer::ItemReady(_) => {
                let record = glyphs(worker.take_item().unwrap());
                let end = record.0.source_utf16.end;
                records.push(record);
                if end == 478 {
                    break;
                }
            }
            MarkdownLineItemAnswer::NeedsFrontier(reason) => {
                panic!("真实第三Font交接仍被拒绝: {reason:?}")
            }
            _ => panic!("真实第三Font交接不能重试旧GCB或提前EOF"),
        }
    }
    assert_eq!(
        requests,
        vec![0..478, 462..464, 464..466, 466..467, 467..468, 468..469]
    );
    assert_eq!(
        records
            .iter()
            .map(|record| record.0.source_utf16.clone())
            .collect::<Vec<_>>(),
        vec![0..256, 256..462, 462..466, 466..468, 468..478]
    );
    for (facts, glyphs) in &records {
        let font = match facts.source_utf16.start {
            462 => &emoji,
            466 => &symbols,
            _ => &fixture.font,
        };
        assert_eq!(*glyphs, oracle(&fixture, font, facts.source_utf16.clone()));
        assert!(!facts.end_of_text);
    }
    drop((worker, fixture, emoji, symbols));
    assert!(source.payload_released());
    assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
    for observer in [emoji_mapped, symbols_mapped] {
        assert!(observer.mapping_drop_completed() && observer.unmap_succeeded());
        assert_eq!(observer.mapped_bytes(), 0);
        assert!(!observer.failed());
    }
}

#[test]
fn middle_font_run_keeps_its_real_child_alive_until_cancel_or_late_source_failure_is_observed() {
    for io in [false, true] {
        let fixture = Fixture::new(
            &format!("{}😀😀{}中\r\n", "é".repeat(462), "é".repeat(10)),
            independent_font(),
        );
        let (emoji, mapped) = emoji_font();
        let cancellation = MarkdownLineComposeCancellation::new();
        let source = fixture.source.resource_observer();
        let backing = fixture.document.original_backing_observer().unwrap();
        let mut worker = MarkdownLineItemCoordinator::new_with_frontier(
            fixture.source.clone(),
            &fixture.authority,
            cancellation.clone(),
        )
        .unwrap();
        let mut held = None;
        for _ in 0..100_000 {
            match advance(&mut worker) {
                MarkdownLineItemAnswer::Yielded => {}
                MarkdownLineItemAnswer::NeedStyle(request) => {
                    let range = request.source_range();
                    let font = if request.continuation_font().is_some() {
                        let probe = worker.font_probe(&request).unwrap();
                        if trial(&fixture, &probe, &fixture.font)
                            .iter()
                            .any(|glyph| glyph.glyph_id == 0)
                        {
                            assert!(
                                trial(&fixture, &probe, &emoji)
                                    .iter()
                                    .all(|glyph| glyph.glyph_id != 0)
                            );
                            emoji.clone()
                        } else {
                            fixture.font.clone()
                        }
                    } else {
                        fixture.font.clone()
                    };
                    worker
                        .provide_style(request.respond(range, font, style()).unwrap())
                        .unwrap();
                }
                MarkdownLineItemAnswer::ItemReady(_) => {
                    let item = worker.take_item().unwrap();
                    if item.facts().source_utf16 == (462..466) {
                        held = Some(item);
                        break;
                    }
                }
                MarkdownLineItemAnswer::NeedsFrontier(reason) => {
                    panic!("取消前Font段被拒绝: {reason:?}")
                }
                _ => panic!("取消前必须得到实际完整回退Font段"),
            }
        }
        let mut held = held.expect("真实Font child已交给原consumer");
        assert!(
            held.glyph(0).unwrap().is_some(),
            "实际child仍持可读输出，尚未归还"
        );
        if io {
            fixture.source.visit_byte_range(0..1, |_| Ok(())).unwrap();
            fixture.document.fail_next_original_reclaim_for_test();
            assert!(matches!(
                fixture.document.reclaim_original_read_windows_for_test(),
                Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                    libc::EIO
                ))
            ));
        }
        cancellation.cancel();
        let failure = worker.step(budget());
        if io {
            assert!(matches!(
                failure,
                Err(MarkdownLineComposeError::Source(
                    MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
                ))
            ));
        } else {
            assert!(matches!(failure, Err(MarkdownLineComposeError::Cancelled)));
        } // Source真实首错优先于取消；两者都不能被PendingConsumer包装为成功。
        drop((worker, held, fixture, emoji));

        assert!(source.payload_released());
        assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0);
        assert_eq!(backing.first_errno(), io.then_some(libc::EIO));
        assert!(
            mapped.mapping_drop_completed()
                && mapped.unmap_succeeded()
                && mapped.mapped_bytes() == 0
                && !mapped.failed()
        );
    }
}

#[test]
fn middle_fallback_run_budget_does_not_create_a_false_font_boundary() {
    let fixture = Fixture::new(
        &format!(
            "{}{}{}中\r\n",
            "é".repeat(462),
            "😀".repeat(129),
            "é".repeat(10)
        ),
        independent_font(),
    );
    let (emoji, mapped) = emoji_font();
    let source = fixture.source.resource_observer();
    let backing = fixture.document.original_backing_observer().unwrap();
    let mut worker = MarkdownLineItemCoordinator::new_with_frontier(
        fixture.source.clone(),
        &fixture.authority,
        MarkdownLineComposeCancellation::new(),
    )
    .unwrap();
    let mut prefix = Vec::new();
    let mut trials = 0;
    let mut rejected = false;
    for _ in 0..100_000 {
        match advance(&mut worker) {
            MarkdownLineItemAnswer::Yielded => {}
            MarkdownLineItemAnswer::NeedStyle(request) => {
                let range = request.source_range();
                let font = if request.continuation_font().is_some() {
                    trials += 1;
                    let probe = worker.font_probe(&request).unwrap();
                    assert!(
                        trial(&fixture, &probe, &fixture.font)
                            .iter()
                            .any(|glyph| glyph.glyph_id == 0)
                    );
                    assert!(
                        trial(&fixture, &probe, &emoji)
                            .iter()
                            .all(|glyph| glyph.glyph_id != 0)
                    );
                    emoji.clone()
                } else {
                    fixture.font.clone()
                };
                worker
                    .provide_style(request.respond(range, font, style()).unwrap())
                    .unwrap();
            }
            MarkdownLineItemAnswer::ItemReady(_) => {
                prefix.push(glyphs(worker.take_item().unwrap()));
            }
            MarkdownLineItemAnswer::NeedsFrontier(reason) => {
                assert_eq!(
                    reason,
                    crate::MarkdownLineFrontierReason::FontRunBeyondBudget
                );
                rejected = true;
                break;
            }
            _ => panic!("连续回退Font不能在有限预算处虚构项末或EOF"),
        }
    }
    assert!(rejected);
    assert_eq!(trials, 129);
    assert_eq!(
        prefix
            .iter()
            .map(|record| record.0.source_utf16.clone())
            .collect::<Vec<_>>(),
        vec![0..256, 256..462]
    );
    let actual = prefix
        .iter()
        .flat_map(|record| record.1.iter().copied())
        .collect::<Vec<_>>();
    assert_eq!(actual, oracle(&fixture, &fixture.font, 0..462));
    drop((worker, fixture, emoji));
    assert!(source.payload_released());
    assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
    assert!(
        mapped.mapping_drop_completed()
            && mapped.unmap_succeeded()
            && mapped.mapped_bytes() == 0
            && !mapped.failed()
    );
}
