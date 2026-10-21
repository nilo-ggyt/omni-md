//! Migrate original graphemes only through the actual item prefix into the same paged file.
use super::*;
use crate::{
    BidiBaseDirection, BidiMemoryBudget, MarkdownBidiParagraph,
    MarkdownComposedGraphemeOrdinalAnswer as Ordinal, MarkdownGraphemeAnswer as Answer,
    MarkdownGraphemeBoundary as Boundary, MarkdownGraphemeQuery as Query, MarkdownLayoutBreakJob,
    MarkdownLineComposeAnswer, MarkdownLineComposer, MarkdownLineParagraphStyle,
    MarkdownSoftLineMeasureJob, MarkdownSoftLineMeasureRequest,
};

fn through(
    store: &mut GlyphStore,
    breaks: &mut MarkdownLayoutBreakJob,
    end: usize,
    writes: usize,
    reads: usize,
) -> (usize, usize) {
    // Drive the real migration with independent read/write quotas. Progress is
    // reported by that operation; no private record-count mirror is needed here.
    let (mut written, mut read) = (0, 0);
    for _ in 0..100_000 {
        let progress = store
            .capture_graphemes_through(breaks, end, writes, reads)
            .unwrap();
        assert!(progress.records <= writes && progress.metadata_units <= reads);
        written += progress.records;
        read += progress.metadata_units;
        if progress.ready {
            return (written, read);
        }
    }
    panic!("真实有限GCB前缀没有完成");
}

struct Prepared {
    fixture: Fixture,
    store: GlyphStore,
    breaks: MarkdownLayoutBreakJob,
    cancellation: MarkdownLineComposeCancellation,
}
impl Prepared {
    /// Commit only the first 0..6 item; later GCB records may interleave with its glyph records.
    fn new(text: &str, extra: Option<usize>) -> Self {
        // This finite prefix lets queries distinguish original graphemes already
        // migrated from later boundaries that remain pending in published output.
        assert!(text.starts_with("aa aa "));
        let fixture = Fixture::new(text);
        let cancellation = MarkdownLineComposeCancellation::new();
        let mut coordinator = fixture.coordinator();
        let mut item = None;
        for _ in 0..100_000 {
            match coordinator
                .step(MarkdownLineComposeBudget::default())
                .unwrap()
                .answer
            {
                MarkdownLineItemAnswer::Yielded => {}
                MarkdownLineItemAnswer::NeedStyle(request) => {
                    // Supply only the requested original prefix through a genuine
                    // registered style, leaving later item work outside this fixture.
                    assert_eq!(request.source_range().start, 0);
                    assert!(request.source_range().end >= 6);
                    coordinator
                        .provide_style(
                            request
                                .respond_registered(
                                    0..6,
                                    &fixture.font,
                                    MarkdownLineComposeStyle {
                                        presentation: Default::default(),
                                        points: 16.0,
                                        language: None,
                                        features: Vec::new(),
                                    },
                                )
                                .unwrap(),
                        )
                        .unwrap();
                }
                MarkdownLineItemAnswer::ItemReady(_) => {
                    item = Some(coordinator.take_item().unwrap());
                    break;
                }
                _ => panic!("有限首item未到Ready"),
            }
        }
        let mut item = item.expect("真实原item");
        assert_eq!(item.facts().source_utf16, 0..6);
        let mut breaks = super::lines::breaks(&fixture);
        let mut store = fixture.store(cancellation.clone());
        assert_eq!(through(&mut store, &mut breaks, 6, 1, 1), (7, 7));
        // Optional prefetch writes later GCB data into the same file before any
        // corresponding line is committed; it must not extend publication authority.
        let proof = capture(&mut store, &mut item);
        if let Some(end) = extra {
            through(&mut store, &mut breaks, end, 2, 1);
        }
        let original_job = item.job.resource_observer();
        drop(item);
        drop(coordinator);
        assert_eq!(original_job.snapshot().files_open, 0);
        // The stored line is committed with actual paragraph direction and the
        // selected measurement permit, after its original shaping job has dropped.
        let mut bidi = MarkdownBidiParagraph::analyze_layout_source(
            fixture.source.clone(),
            &fixture.source.authority(),
            0..fixture.source.utf16_len(),
            BidiBaseDirection::Auto,
            BidiMemoryBudget::new(65536).unwrap(),
        )
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
                    width_26_6: 40 * 64,
                    cancellation: cancellation.measure(),
                },
            )
            .unwrap()
        };
        let selected = super::providers::selected(&mut measure, &mut store, &proof, &mut breaks);
        assert!(matches!(
            selected,
            crate::MarkdownSoftLineMeasureChoice::MeasuredBreak(_)
        ));
        let permit = {
            let mut provider = store.borrow(&proof).unwrap();
            measure.selected_permit(&mut provider).unwrap()
        };
        let line = store
            .append_selected_line(&proof, &permit, &mut bidi, 0)
            .unwrap();
        assert!(line.source_utf16.end > 0 && line.source_utf16.end < 6);
        drop((measure, bidi));
        Self {
            fixture,
            store,
            breaks,
            cancellation,
        }
    }
}

#[test]
fn migration_charges_real_reads_and_writes_separately_at_quota_one() {
    // Different asymmetric quotas must converge to the same actual migrated
    // prefix, without treating a fully ready break job as fully copied output.
    for (writes, reads) in [(1, 3), (3, 1), (256, 256)] {
        let fixture = Fixture::new(&"aa ".repeat(32));
        let mut breaks = super::lines::breaks(&fixture);
        let mut store = fixture.store(MarkdownLineComposeCancellation::new());
        assert_eq!(
            through(&mut store, &mut breaks, 30, writes, reads),
            (31, 31)
        );
        let directory = store.grapheme_directory().unwrap();
        assert_eq!((directory.count, directory.covered.utf16), (31, 30));
        // The original job has 96 ASCII scalars plus true EOF, but only 31
        // boundaries through offset 30 were migrated. Known total is not complete.
        let total = directory.total.expect("原完整Ready表必须保留已知总数");
        assert_eq!(total, 97);
        assert!(!directory.complete && total > directory.count);
        let observer = store.observer();
        store.close().unwrap();
        let actual = observer.snapshot();
        assert!(!actual.file_open && actual.mapped_bytes == 0 && !actual.cleanup_failed);
    }
}

#[test]
fn incomplete_queries_are_pending_and_legacy_ordinal_does_not_poison_the_reader() {
    // A known prefix boundary may be queried while its successor is still pending.
    // The ordinal convenience API must preserve Pending rather than poison the file.
    let prepared = Prepared::new("aa aa aa aa aa aa ", None);
    let mut output = prepared.store.into_output().unwrap();
    assert_eq!(
        output.grapheme_ordinal_answer(6).unwrap(),
        Ordinal::Known(Some(6))
    );
    assert!(matches!(
        output.grapheme_query(6, Query::Next).unwrap(),
        Answer::Pending
    ));
    for query in [Query::IsBoundary, Query::Previous, Query::Next] {
        assert!(matches!(
            output.grapheme_query(7, query).unwrap(),
            Answer::Pending
        ));
    }
    assert_eq!(output.grapheme_ordinal_answer(7).unwrap(), Ordinal::Pending);
    assert!(matches!(
        output.grapheme_ordinal(7),
        Err(Error::GraphemePrefixPending)
    ));
    // The same reader remains healthy after that recoverable pending result and
    // can still answer original-prefix predecessor and BOF queries.
    output
        .require(&prepared.fixture.source.authority())
        .unwrap();
    assert_eq!(output.grapheme_ordinal(0).unwrap(), Some(0));
    assert!(matches!(
        output.grapheme_query(0, Query::Previous).unwrap(),
        Answer::Known(None)
    ));
    assert!(
        matches!(output.grapheme_query(6, Query::Previous).unwrap(), Answer::Known(Some(value)) if value.utf16 == 5)
    );
    output.close().unwrap();
}

#[test]
fn interleaved_records_preserve_original_non_bmp_and_multi_scalar_global_ordinals() {
    // Non-BMP scalars and combining sequences make UTF-16 offsets differ from
    // global grapheme ordinals; the independent original break job is the oracle.
    let text = "aa aa a\u{301} \u{1f16a} b \u{1f16b} z";
    let end = text.encode_utf16().count();
    let mut prepared = Prepared::new(text, Some(end));
    let auth = prepared.fixture.source.authority();
    let count = prepared
        .breaks
        .ready_grapheme_count(&auth)
        .unwrap()
        .unwrap();
    // This small fixture may hold a finite comparison array. Production continues
    // to read bounded records from the original migrated index.
    let mut oracle = vec![Boundary { byte: 0, utf16: 0 }; count];
    assert!(
        prepared
            .breaks
            .read_ready_graphemes(&auth, 0..count, &mut oracle)
            .unwrap()
    );
    let mut output = prepared.store.into_output().unwrap();
    assert!(count < end + 1, "不能将UTF16长度当成全局ordinal");
    for offset in 0..=end {
        let expected = oracle.iter().position(|value| value.utf16 == offset);
        assert_eq!(output.grapheme_ordinal(offset).unwrap(), expected);
        for query in [Query::Previous, Query::Next, Query::IsBoundary] {
            let expected = prepared
                .breaks
                .grapheme_query(&auth, offset, query)
                .unwrap();
            assert_eq!(output.grapheme_query(offset, query).unwrap(), expected);
        }
    }
    assert!(matches!(
        output.grapheme_query(end, Query::Next).unwrap(),
        Answer::Known(None)
    ));
    output.close().unwrap();
}

#[cfg(feature = "paged-shaping-frontier")]
#[test]
fn uncommitted_prefetch_cannot_extend_old_or_new_snapshot_past_the_last_line_end() {
    // Publish a real committed prefix, then prefetch the remaining original GCBs.
    // Neither the existing reader nor a new same-line snapshot gains extra range.
    let mut prepared = Prepared::new("aa aa aa aa aa aa ", None);
    let mut old = prepared.store.publish_ready_prefix().unwrap().unwrap();
    let old_receipt = old.resource_observer();
    through(
        &mut prepared.store,
        &mut prepared.breaks,
        prepared.fixture.source.utf16_len(),
        1,
        1,
    );
    assert!(prepared.store.grapheme_directory().unwrap().complete);
    let mut same_line = prepared.store.publish_ready_prefix().unwrap().unwrap();
    for reader in [&mut old, &mut same_line] {
        assert_eq!(reader.grapheme_ordinal(6).unwrap(), Some(6));
        assert_eq!(reader.grapheme_ordinal_answer(7).unwrap(), Ordinal::Pending);
        assert!(matches!(
            reader.grapheme_query(6, Query::Next).unwrap(),
            Answer::Pending
        ));
        assert!(reader.last_line().unwrap().is_some());
    }
    same_line.close().unwrap();
    old.close().unwrap();
    assert!(!old_receipt.snapshot().file_open);
    // Consuming the writer also publishes only its last committed directory;
    // later records existing in the file do not authorize an extended snapshot.
    let mut final_prefix = prepared.store.into_output().unwrap();
    assert_eq!(
        final_prefix.grapheme_ordinal_answer(7).unwrap(),
        Ordinal::Pending
    );
    final_prefix.close().unwrap();
}

fn complete(fixture: &Fixture) -> MarkdownComposedLines {
    // Exercise the real composer with unit quotas, answering only its original
    // style/control requests. Yielded and LineReady do not imply source completion.
    let mut worker = MarkdownLineComposer::new(
        fixture.source.clone(),
        &fixture.source.authority(),
        fixture.catalog.clone(),
        200 * 64,
        MarkdownLineComposeCancellation::new(),
    )
    .unwrap();
    for _ in 0..100_000 {
        let progress = worker
            .step(MarkdownLineComposeBudget {
                bytes: 1,
                scalars: 1,
                records: 1,
                metadata_units: 1,
            })
            .unwrap();
        assert!(progress.bytes <= 1 && progress.records <= 1 && progress.metadata_units <= 1);
        match progress.answer {
            MarkdownLineComposeAnswer::Yielded | MarkdownLineComposeAnswer::LineReady(_) => {}
            MarkdownLineComposeAnswer::NeedStyle(request) => {
                // The response preserves the complete requested source range and
                // actual font registration rather than bypassing a dependency.
                let range = request.source_range();
                worker
                    .provide_style(
                        request
                            .respond_registered(
                                range,
                                &fixture.font,
                                MarkdownLineComposeStyle {
                                    presentation: Default::default(),
                                    points: 16.0,
                                    language: None,
                                    features: Vec::new(),
                                },
                            )
                            .unwrap(),
                    )
                    .unwrap();
            }
            MarkdownLineComposeAnswer::NeedsControl(request) => {
                // Supply the real paragraph tab policy for this control request;
                // controls retain original CR/LF/EOF and grapheme semantics.
                worker
                    .provide_control(
                        request
                            .respond(
                                &fixture.font,
                                16.0,
                                MarkdownLineParagraphStyle {
                                    tab_stops: Vec::new(),
                                    default_tab_interval_26_6: Some(8 * 64),
                                },
                            )
                            .unwrap(),
                    )
                    .unwrap();
            }
            MarkdownLineComposeAnswer::Complete => return worker.into_output().unwrap(),
            _ => panic!("有限真实控制夹具未完成"),
        }
    }
    panic!("有限控制worker没有真实EOF");
}

#[test]
fn empty_and_terminal_controls_keep_true_eof_and_crlf_is_one_original_grapheme() {
    // Compare all original boundary queries, including empty input and terminal
    // controls. CRLF remains one grapheme even under one-record producer quanta.
    for text in ["", "\n", "\r\n", "a\tb\n", "a\u{85}b\u{2028}"] {
        let fixture = Fixture::new(text);
        let mut breaks = super::lines::breaks(&fixture);
        let mut output = complete(&fixture);
        for offset in 0..=fixture.source.utf16_len() {
            for query in [Query::Previous, Query::Next, Query::IsBoundary] {
                assert_eq!(
                    output.grapheme_query(offset, query).unwrap(),
                    breaks
                        .grapheme_query(&fixture.source.authority(), offset, query)
                        .unwrap()
                );
            }
        }
        let end = fixture.source.utf16_len();
        assert!(matches!(
            output.grapheme_query(end, Query::Next).unwrap(),
            Answer::Known(None)
        ));
        let observer = output.resource_observer();
        output.close().unwrap();
        let actual = observer.snapshot();
        assert!(!actual.file_open && actual.mapped_bytes == 0 && !actual.reader_failed);
    }
}

#[test]
fn cancellation_with_one_buffered_original_record_closes_the_unique_writer() {
    // Make actual migration progress with one buffered boundary, then revoke that
    // same request before another read/write quantum can publish more data.
    let fixture = Fixture::new("aa aa aa aa ");
    let source = fixture.source.resource_observer();
    let backing = fixture.source.original_backing_observer().unwrap();
    let mut breaks = super::lines::breaks(&fixture);
    let cancellation = MarkdownLineComposeCancellation::new();
    let mut store = fixture.store(cancellation.clone());
    let observer = store.observer();
    let progress = store
        .capture_graphemes_through(&mut breaks, 6, 1, 2)
        .unwrap();
    assert_eq!(
        (progress.records, progress.metadata_units, progress.ready),
        (1, 2, false)
    );
    cancellation.cancel();
    assert!(matches!(
        store.capture_graphemes_through(&mut breaks, 6, 1, 1),
        Err(Error::Cancelled)
    ));
    let actual = observer.snapshot();
    assert!(!actual.file_open && actual.mapped_bytes == 0 && !actual.cleanup_failed);
    // Drop the remaining original break/source owners before checking actual
    // payload release and successful unmap, not merely writer cancellation state.
    drop((store, breaks, fixture));
    assert!(source.payload_released() && backing.unmap_succeeded() && backing.mapped_bytes() == 0);
}

#[cfg(feature = "paged-shaping-frontier")]
#[test]
fn real_source_failure_precedes_cancel_and_pending_for_both_reader_and_writer() {
    // Keep a real published reader and active writer from the same original
    // source, so both paths must retain its first failure over Pending/cancellation.
    let mut prepared = Prepared::new("aa aa aa aa aa aa ", None);
    let source = prepared.fixture.source.resource_observer();
    let backing = prepared.fixture.source.original_backing_observer().unwrap();
    let writer = prepared.store.observer();
    let mut reader = prepared.store.publish_ready_prefix().unwrap().unwrap();
    let reader_receipt = reader.resource_observer();
    // Consume the existing reclaim EIO injection through a real resident-source
    // operation; this is not evidence of a naturally occurring kernel failure.
    prepared
        .fixture
        .document
        .text_snapshot()
        .visit_byte_range(0..1, |_| Ok(()))
        .unwrap();
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
    );
    prepared.cancellation.cancel();
    // A pending offset must return the original source cause before considering
    // missing prefix data or the subsequently cancelled request.
    assert!(matches!(
        reader.grapheme_ordinal_answer(7),
        Err(Error::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
    assert!(matches!(
        prepared
            .store
            .capture_graphemes_through(&mut prepared.breaks, 12, 1, 1),
        Err(Error::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
    assert!(!writer.snapshot().file_open && writer.snapshot().mapped_bytes == 0);
    assert!(!reader_receipt.snapshot().file_open && reader_receipt.snapshot().mapped_bytes == 0);
    drop((reader, prepared));
    assert!(source.payload_released() && backing.unmap_succeeded() && backing.mapped_bytes() == 0);
}

#[test]
fn same_bytes_foreign_break_job_cannot_sign_any_store_grapheme_record() {
    // Two equal-text files have different captures. The foreign break job cannot
    // authorize even the first migrated record and the real writer must close.
    let fixture = Fixture::new("aa aa aa ");
    let foreign = Fixture::new("aa aa aa ");
    let mut breaks = super::lines::breaks(&foreign);
    let mut store = fixture.store(MarkdownLineComposeCancellation::new());
    let observer = store.observer();
    assert!(matches!(
        store.capture_graphemes_through(&mut breaks, 6, 1, 1),
        Err(Error::Break(
            crate::MarkdownLayoutBreakError::SourceMismatch
        ))
    ));
    assert!(!observer.snapshot().file_open && observer.snapshot().mapped_bytes == 0);
}
