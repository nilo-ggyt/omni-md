//! A next-line start requires a selected and committed LineEnd, not a coordinate or bare grapheme.
use super::*;
use crate::markdown_soft_line_measure::provider::SelectedLineStart;
use crate::{
    BidiBaseDirection, BidiMemoryBudget, MarkdownBidiParagraph, MarkdownLayoutBreakJob,
    MarkdownSoftLineMeasureBudget, MarkdownSoftLineMeasureCancellation,
    MarkdownSoftLineMeasureChoice, MarkdownSoftLineMeasureError, MarkdownSoftLineMeasureJob,
    MarkdownSoftLineMeasureRequest,
};

struct Prepared {
    // This fixture retains the genuine source, writer, break/bidi jobs and original
    // measure until each test explicitly transfers or retires their real owners.
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
        // Only synthetic finite source is materialized here. The long unbroken
        // case uses the real frontier and a real system font to force an emergency.
        let text = if emergency {
            "a".repeat(4099)
        } else {
            "aa aa aa aa aa aa aa aa aa aa aa aa".to_owned()
        }; // The production store never retains this fixture's whole-text String.
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
        } // This is an actual-font positive case; production permissions do not whitelist its name.
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
        // Persist the item and grapheme records, then release its original job
        // before measuring through the stored provider.
        let original = item.job.resource_observer();
        let mut store = fixture.store(cancellation.clone());
        let mut breaks = super::lines::breaks(&fixture);
        while !store.capture_graphemes(&mut breaks, 3).unwrap() {}
        let proof = capture(&mut store, &mut item);
        drop(item);
        drop(coordinator);
        assert_eq!(original.snapshot().files_open, 0);
        assert_eq!(original.snapshot().mapped_bytes, 0);
        // The committed line still needs the same original paragraph direction;
        // stored glyph availability is not a substitute for bidi authority.
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
        // Require the genuine selection variant for each fixture. The selected
        // endpoint must remain strictly inside the original source, not at fake EOF.
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
        assert!(end > 0 && end < fixture.source.utf16_len());
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
        // A selected permit gains next-start authority only through the actual
        // LineEnd append; constructing a permit alone does not commit anything.
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
        // Borrow the original proof for this one-shot handoff. Ownership of the
        // resulting ticket belongs to its consumer, not to the stored coordinate.
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
        // Consume the original ticket while preserving exact source, font, break
        // and cancellation identities; an equal false cancel bit is not equivalent.
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
    // An emergency GCB is not necessarily a natural line-break opportunity. The
    // ordinary constructor must reject it before a committed ticket exists.
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
        )); // Keep the ordinary constructor's natural-opportunity contract intact.
    } // End the provider borrow before the actual LineEnd commit.
    drop(raw);
    prepared.commit();
    let ticket = prepared.issue().unwrap();
    assert!(matches!(
        prepared.issue(),
        Err(MarkdownSoftLineMeasureError::InvalidRequest)
    )); // The original measure cannot issue a second ticket, even if the first is discarded.
    drop(prepared.measure.take());
    // A committed ticket survives the original measure's destruction and permits
    // genuine progress from that exact next-line position.
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
    // Neither an uncommitted selected GCB nor a nearby coordinate grants the
    // original single-use start; both must fail without broadening its coverage.
    let mut uncommitted = Prepared::new(true);
    assert!(matches!(
        uncommitted.issue(),
        Err(MarkdownSoftLineMeasureError::InvalidRequest)
    )); // No actual LineEnd has been written for this selection.
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
    // Persist equal-source glyphs in another actual writer. Its provider is still
    // outside the original committed line family and may not consume that ticket.
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
    // First reject a different request identity while both cancel states are false,
    // then independently prove revocation by cancelling the real original request.
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
    )); // Equal current booleans are not the same revocation authority.
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
    // Advance the actual committed line family before consuming the older ticket;
    // a stale coordinate must not reopen a line behind that real publication.
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
    }; // A natural opportunity permits this separate ordinary measure without copying the ticket.
    assert!(matches!(
        super::providers::selected(
            &mut next,
            &mut prepared.store,
            &prepared.proof,
            &mut prepared.breaks,
        ),
        MarkdownSoftLineMeasureChoice::MeasuredBreak(_)
    ));
    // Append the next genuinely selected line, then try the still-unconsumed old
    // ticket against the store's now-later committed boundary.
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
    // Keep the committed ticket after retiring its original measure. Consume the
    // existing failure injection via actual source reclaim before later cancellation.
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
        ); // The existing EIO injection was consumed by real reclaim; it is not a natural kernel fault.
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
    // Release the whole fixture owner graph before checking real writer, source
    // payload and mapping receipts; a consumed ticket alone proves no cleanup.
    drop(prepared);
    let released = observer.snapshot();
    assert!(!released.file_open && released.mapped_bytes == 0 && !released.cleanup_failed);
    assert!(source.payload_released() && backing.unmap_succeeded());
}
