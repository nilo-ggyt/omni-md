//! 真实原依赖签发的字体票只覆盖一个GCB，外部副本不延长样式阶段的权限。

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
        // 固定次数目录读取不创建新GCB/Script/Bidi资源；真实resource scope保持同一组依赖。
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
    assert!(!observer.payload_released()); // 在途原packet仍有同Source读claim；scope撤权不伪称已经unmap。
    std::thread::spawn(move || {
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
