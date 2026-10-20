//! 行首许可来自真实选中与LineEnd提交；同源坐标、普通GCB或未提交选择都不能替代。
use super::*;
use crate::markdown_soft_line_measure::provider::SelectedLineStart;
use crate::{
    BidiBaseDirection, BidiMemoryBudget, MarkdownBidiParagraph, MarkdownLayoutBreakJob,
    MarkdownSoftLineMeasureBudget, MarkdownSoftLineMeasureCancellation,
    MarkdownSoftLineMeasureChoice, MarkdownSoftLineMeasureError, MarkdownSoftLineMeasureJob,
    MarkdownSoftLineMeasureRequest,
};

struct Prepared {
    fixture: Fixture,
    cancellation: MarkdownLineComposeCancellation,
    store: GlyphStore,
    proof: StoredItemProof,
    breaks: MarkdownLayoutBreakJob,
    bidi: MarkdownBidiParagraph,
    measure: Option<MarkdownSoftLineMeasureJob>,
    end: usize,
}

impl Prepared {
    fn new(emergency: bool) -> Self {
        let text = if emergency {
            "a".repeat(4099)
        } else {
            "aa aa aa aa aa aa aa aa aa aa aa aa".to_owned()
        }; // 只在这个有限测试夹具生成正文；产品Store不保全文String。
        let mut fixture = Fixture::new(&text);
        if emergency {
            let bytes = crate::MarkdownLayoutFontBytes::capture_file(
                std::path::Path::new("/System/Library/Fonts/SFNSMono.ttf"),
                32 * 1024 * 1024,
            )
            .unwrap();
            let font =
                crate::MarkdownLayoutFont::resolve(bytes.blob(), ".SFNSMono-Regular", &[]).unwrap();
            fixture.font = fixture.catalog.register(&font).unwrap();
        } // 这是实际系统字体正控，生产计划证书没有字体名称白名单。
        let cancellation = MarkdownLineComposeCancellation::new();
        let mut coordinator = if emergency {
            MarkdownLineItemCoordinator::new_with_frontier(
                fixture.source.clone(),
                &fixture.source.authority(),
                cancellation.clone(),
            )
            .unwrap()
        } else {
            fixture.coordinator()
        };
        let mut item = fixture
            .next_with_style(
                &mut coordinator,
                MarkdownLineComposeStyle {
                    presentation: Default::default(),
                    points: 16.0,
                    language: None,
                    features: Vec::new(),
                },
            )
            .unwrap();
        let original = item.job.resource_observer();
        let mut store = fixture.store(cancellation.clone());
        let mut breaks = super::lines::breaks(&fixture);
        while !store.capture_graphemes(&mut breaks, 3).unwrap() {}
        let proof = capture(&mut store, &mut item);
        drop(item);
        drop(coordinator);
        assert_eq!(original.snapshot().files_open, 0);
        assert_eq!(original.snapshot().mapped_bytes, 0);
        let bidi = MarkdownBidiParagraph::analyze_layout_source(
            fixture.source.clone(),
            &fixture.source.authority(),
            0..fixture.source.utf16_len(),
            BidiBaseDirection::Auto,
            BidiMemoryBudget::new(65536).unwrap(),
        )
        .unwrap();
        bidi.require_layout_source(&fixture.source.authority())
            .unwrap();
        let mut measure = {
            let mut provider = store.borrow(&proof).unwrap();
            MarkdownSoftLineMeasureJob::new_with_provider(
                fixture.source.clone(),
                &fixture.source.authority(),
                &fixture.font.font().authority(),
                &mut provider,
                &mut breaks,
                MarkdownSoftLineMeasureRequest {
                    start_utf16: 0,
                    width_26_6: 80 * 64,
                    cancellation: cancellation.measure(),
                },
            )
            .unwrap()
        };
        let choice = super::providers::selected(&mut measure, &mut store, &proof, &mut breaks);
        let end = match choice {
            MarkdownSoftLineMeasureChoice::MeasuredBreak(value) if !emergency => {
                value.source_utf16.end
            }
            MarkdownSoftLineMeasureChoice::MeasuredEmergencyBreak { source_utf16, .. }
                if emergency =>
            {
                source_utf16.end
            }
            other => panic!("真实原选择未取得预期安全行尾: {other:?}"),
        };
        assert!(end > 0 && end < store.source_end);
        Self {
            fixture,
            cancellation,
            store,
            proof,
            breaks,
            bidi,
            measure: Some(measure),
            end,
        }
    }

    fn commit(&mut self) {
        let measure = self.measure.as_ref().unwrap();
        let permit = {
            let mut provider = self.store.borrow(&self.proof).unwrap();
            measure.selected_permit(&mut provider).unwrap()
        };
        let facts = self
            .store
            .append_selected_line(&self.proof, &permit, &mut self.bidi, 0)
            .unwrap();
        assert_eq!(facts.source_utf16.end, self.end);
    }

    fn issue(&mut self) -> std::result::Result<SelectedLineStart, MarkdownSoftLineMeasureError> {
        let mut provider = self.store.borrow(&self.proof).unwrap();
        let permit = self
            .measure
            .as_ref()
            .unwrap()
            .selected_permit(&mut provider)?;
        permit.next_start(&mut provider)
    }

    fn restart(
        &mut self,
        ticket: SelectedLineStart,
        start: usize,
        cancellation: MarkdownSoftLineMeasureCancellation,
    ) -> std::result::Result<MarkdownSoftLineMeasureJob, MarkdownSoftLineMeasureError> {
        let mut provider = self.store.borrow(&self.proof).unwrap();
        MarkdownSoftLineMeasureJob::after_selected_start(
            self.fixture.source.clone(),
            &self.fixture.source.authority(),
            &self.fixture.font.font().authority(),
            &mut provider,
            &mut self.breaks,
            MarkdownSoftLineMeasureRequest {
                start_utf16: start,
                width_26_6: 80 * 64,
                cancellation,
            },
            ticket,
        )
    }
}

#[test]
fn actual_emergency_commit_survives_old_measure_drop_but_raw_gcb_is_not_a_line_start() {
    let mut prepared = Prepared::new(true);
    let mut raw = {
        let mut provider = prepared.store.borrow(&prepared.proof).unwrap();
        MarkdownSoftLineMeasureJob::new_with_provider(
            prepared.fixture.source.clone(),
            &prepared.fixture.source.authority(),
            &prepared.fixture.font.font().authority(),
            &mut provider,
            &mut prepared.breaks,
            MarkdownSoftLineMeasureRequest {
                start_utf16: prepared.end,
                width_26_6: 80 * 64,
                cancellation: prepared.cancellation.measure(),
            },
        )
        .unwrap()
    };
    {
        let mut provider = prepared.store.borrow(&prepared.proof).unwrap();
        assert!(matches!(
            raw.step_with_provider(
                &mut provider,
                &mut prepared.breaks,
                MarkdownSoftLineMeasureBudget::default(),
            ),
            Err(MarkdownSoftLineMeasureError::InvalidRequest)
        )); // 普通new仍要求自然机会；不能通过放宽At(None)让这项负控通过。
    } // 结束同Store借用后才提交真实下一行，不把无资源的借用适配器当作析构收据。
    drop(raw);
    prepared.commit();
    let ticket = prepared.issue().unwrap();
    assert!(matches!(
        prepared.issue(),
        Err(MarkdownSoftLineMeasureError::InvalidRequest)
    )); // 同一原Measure不可签第二份票，丢票也不会隐式重新授权。
    drop(prepared.measure.take());
    let mut next = prepared
        .restart(ticket, prepared.end, prepared.cancellation.measure())
        .unwrap();
    let choice = super::providers::selected(
        &mut next,
        &mut prepared.store,
        &prepared.proof,
        &mut prepared.breaks,
    );
    assert!(matches!(choice,
        MarkdownSoftLineMeasureChoice::MeasuredEmergencyBreak { source_utf16, .. }
            if source_utf16.start == prepared.end && source_utf16.end > prepared.end
    ));
}

#[test]
fn uncommitted_selection_and_inexact_requested_start_cannot_issue_or_consume_a_ticket() {
    let mut uncommitted = Prepared::new(true);
    assert!(matches!(
        uncommitted.issue(),
        Err(MarkdownSoftLineMeasureError::InvalidRequest)
    )); // 实际选择尚未写LineEnd，未提交GCB本身不产生下一行许可。
    let mut committed = Prepared::new(false);
    committed.commit();
    let ticket = committed.issue().unwrap();
    assert!(matches!(
        committed.restart(ticket, committed.end + 1, committed.cancellation.measure()),
        Err(MarkdownSoftLineMeasureError::SourceMismatch)
    ));
}

#[test]
fn same_source_foreign_store_is_not_the_original_committed_line_family() {
    let mut prepared = Prepared::new(false);
    prepared.commit();
    let ticket = prepared.issue().unwrap();
    let mut coordinator = prepared.fixture.coordinator();
    let mut item = prepared.fixture.next(&mut coordinator).unwrap();
    let mut foreign = prepared.fixture.store(prepared.cancellation.clone());
    let proof = capture(&mut foreign, &mut item);
    drop(item);
    let mut provider = foreign.borrow(&proof).unwrap();
    assert!(matches!(
        MarkdownSoftLineMeasureJob::after_selected_start(
            prepared.fixture.source.clone(),
            &prepared.fixture.source.authority(),
            &prepared.fixture.font.font().authority(),
            &mut provider,
            &mut prepared.breaks,
            MarkdownSoftLineMeasureRequest {
                start_utf16: prepared.end,
                width_26_6: 80 * 64,
                cancellation: prepared.cancellation.measure(),
            },
            ticket,
        ),
        Err(MarkdownSoftLineMeasureError::JobMismatch)
    ));
}

#[test]
fn a_different_false_cancel_bit_and_actual_cancellation_both_revoke_the_start() {
    let mut different = Prepared::new(false);
    different.commit();
    let ticket = different.issue().unwrap();
    assert!(matches!(
        different.restart(
            ticket,
            different.end,
            MarkdownSoftLineMeasureCancellation::new()
        ),
        Err(MarkdownSoftLineMeasureError::SourceMismatch)
    )); // 两个位当前都是false也不是同一个撤权请求。
    let mut cancelled = Prepared::new(false);
    cancelled.commit();
    let ticket = cancelled.issue().unwrap();
    let mut provider = cancelled.store.borrow(&cancelled.proof).unwrap();
    cancelled.cancellation.cancel();
    assert!(matches!(
        MarkdownSoftLineMeasureJob::after_selected_start(
            cancelled.fixture.source.clone(),
            &cancelled.fixture.source.authority(),
            &cancelled.fixture.font.font().authority(),
            &mut provider,
            &mut cancelled.breaks,
            MarkdownSoftLineMeasureRequest {
                start_utf16: cancelled.end,
                width_26_6: 80 * 64,
                cancellation: cancelled.cancellation.measure(),
            },
            ticket,
        ),
        Err(MarkdownSoftLineMeasureError::Cancelled)
    ));
}

#[test]
fn a_later_real_line_end_prevents_an_old_ticket_from_reopening_its_start() {
    let mut prepared = Prepared::new(false);
    prepared.commit();
    let ticket = prepared.issue().unwrap();
    let mut next = {
        let mut provider = prepared.store.borrow(&prepared.proof).unwrap();
        MarkdownSoftLineMeasureJob::new_with_provider(
            prepared.fixture.source.clone(),
            &prepared.fixture.source.authority(),
            &prepared.fixture.font.font().authority(),
            &mut provider,
            &mut prepared.breaks,
            MarkdownSoftLineMeasureRequest {
                start_utf16: prepared.end,
                width_26_6: 80 * 64,
                cancellation: prepared.cancellation.measure(),
            },
        )
        .unwrap()
    }; // 自然机会允许独立旧API创建此对照；不复制或手造原单次票。
    assert!(matches!(
        super::providers::selected(
            &mut next,
            &mut prepared.store,
            &prepared.proof,
            &mut prepared.breaks,
        ),
        MarkdownSoftLineMeasureChoice::MeasuredBreak(_)
    ));
    let permit = {
        let mut provider = prepared.store.borrow(&prepared.proof).unwrap();
        next.selected_permit(&mut provider).unwrap()
    };
    let facts = prepared
        .store
        .append_selected_line(&prepared.proof, &permit, &mut prepared.bidi, 0)
        .unwrap();
    assert!(facts.source_utf16.end > prepared.end);
    assert!(matches!(
        prepared.restart(ticket, prepared.end, prepared.cancellation.measure()),
        Err(MarkdownSoftLineMeasureError::InvalidRequest
            | MarkdownSoftLineMeasureError::InconsistentCoverage)
    ));
}

#[test]
fn original_source_io_failure_precedes_cancel_and_all_real_resources_are_returned() {
    let mut prepared = Prepared::new(false);
    prepared.commit();
    let ticket = prepared.issue().unwrap();
    drop(prepared.measure.take());
    let observer = prepared.store.observer();
    let backing = prepared.fixture.source.original_backing_observer().unwrap();
    let source = prepared.fixture.source.resource_observer();
    {
        let mut provider = prepared.store.borrow(&prepared.proof).unwrap();
        prepared
            .fixture
            .document
            .fail_next_original_reclaim_for_test();
        assert_eq!(
            prepared
                .fixture
                .document
                .reclaim_original_read_windows_for_test(),
            Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                libc::EIO
            ))
        ); // 故障必须发生在真实驻留页归还操作，而不是只设置未消费的测试标志。
        prepared.cancellation.cancel();
        assert!(matches!(
            MarkdownSoftLineMeasureJob::after_selected_start(
                prepared.fixture.source.clone(),
                &prepared.fixture.source.authority(),
                &prepared.fixture.font.font().authority(),
                &mut provider,
                &mut prepared.breaks,
                MarkdownSoftLineMeasureRequest {
                    start_utf16: prepared.end,
                    width_26_6: 80 * 64,
                    cancellation: prepared.cancellation.measure(),
                },
                ticket,
            ),
            Err(MarkdownSoftLineMeasureError::Source(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
            ))
        ));
    }
    drop(prepared);
    let released = observer.snapshot();
    assert!(!released.file_open && released.mapped_bytes == 0 && !released.cleanup_failed);
    assert!(source.payload_released() && backing.unmap_succeeded());
}
