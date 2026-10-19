//! 原GCB只按实际item前缀迁入同FD；有限oracle可保数组，生产没有第二索引或正文副本。
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
    let (mut written, mut read) = (0, 0);
    for _ in 0..100_000 {
        let before = store.record_count().unwrap();
        let progress = store
            .capture_graphemes_through(breaks, end, writes, reads)
            .unwrap();
        assert!(progress.records <= writes && progress.metadata_units <= reads);
        assert_eq!(store.record_count().unwrap() - before, progress.records);
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
    /// 首item只覆盖0..6；后继GCB可在glyph目录之后、LineEnd之前交错写入。
    fn new(text: &str, extra: Option<usize>) -> Self {
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
                    assert_eq!(request.source_range().start, 0);
                    assert!(request.source_range().end >= 6);
                    coordinator
                        .provide_style(
                            request
                                .respond_registered(
                                    0..6,
                                    &fixture.font,
                                    MarkdownLineComposeStyle {
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
        let proof = capture(&mut store, &mut item);
        if let Some(end) = extra {
            through(&mut store, &mut breaks, end, 2, 1);
        }
        let original_job = item.job.resource_observer();
        drop(item);
        drop(coordinator);
        assert_eq!(original_job.snapshot().files_open, 0);
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
        // 此夹具的原BreakJob已真正Ready：96个ASCII标量加真实EOF共97条边界；
        // Store只复制0..30的31条，已知完整总数和已迁入前缀不能混为一个Ready位。
        let total = directory.total.expect("原完整Ready表必须保留已知总数");
        assert_eq!(total, 97);
        assert!(!directory.complete && total > directory.count);
        assert_eq!(store.record_count().unwrap(), 31);
        let observer = store.observer();
        store.close().unwrap();
        let actual = observer.snapshot();
        assert!(!actual.file_open && actual.mapped_bytes == 0 && !actual.cleanup_failed);
    }
}

#[test]
fn incomplete_queries_are_pending_and_legacy_ordinal_does_not_poison_the_reader() {
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
    let text = "aa aa a\u{301} \u{1f16a} b \u{1f16b} z";
    let end = text.encode_utf16().count();
    let mut prepared = Prepared::new(text, Some(end));
    let auth = prepared.fixture.source.authority();
    let count = prepared
        .breaks
        .ready_grapheme_count(&auth)
        .unwrap()
        .unwrap();
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
    // 消费writer也只能交最近已提交目录，不能把文件中更晚存在的行当成快照权限。
    let mut final_prefix = prepared.store.into_output().unwrap();
    assert_eq!(
        final_prefix.grapheme_ordinal_answer(7).unwrap(),
        Ordinal::Pending
    );
    final_prefix.close().unwrap();
}

fn complete(fixture: &Fixture) -> MarkdownComposedLines {
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
                let range = request.source_range();
                worker
                    .provide_style(
                        request
                            .respond_registered(
                                range,
                                &fixture.font,
                                MarkdownLineComposeStyle {
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
    drop((store, breaks, fixture));
    assert!(source.payload_released() && backing.unmap_succeeded() && backing.mapped_bytes() == 0);
}

#[cfg(feature = "paged-shaping-frontier")]
#[test]
fn real_source_failure_precedes_cancel_and_pending_for_both_reader_and_writer() {
    let mut prepared = Prepared::new("aa aa aa aa aa aa ", None);
    let source = prepared.fixture.source.resource_observer();
    let backing = prepared.fixture.source.original_backing_observer().unwrap();
    let writer = prepared.store.observer();
    let mut reader = prepared.store.publish_ready_prefix().unwrap().unwrap();
    let reader_receipt = reader.resource_observer();
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
    assert!(store.graphemes.is_none());
    assert!(!observer.snapshot().file_open && observer.snapshot().mapped_bytes == 0);
}

#[test]
fn fixed_skip_links_bound_deep_real_record_reads_and_reject_corrupt_ranks() {
    let fixture = Fixture::new(&"a ".repeat(4096));
    let mut breaks = super::lines::breaks(&fixture);
    let mut store = fixture.store(MarkdownLineComposeCancellation::new());
    through(
        &mut store,
        &mut breaks,
        fixture.source.utf16_len(),
        256,
        256,
    );
    let directory = store.grapheme_directory().unwrap();
    let mut reads = 0;
    let value = graphemes::ordinal_boundary(
        directory,
        1,
        fixture.source.byte_len(),
        fixture.source.utf16_len(),
        |record| {
            reads += 1;
            Ok(store.index.as_mut().unwrap().read(record)?)
        },
    )
    .unwrap();
    assert_eq!(value.unwrap().utf16, 1);
    assert!(reads < 512 && reads < directory.count / 2);
    let mut reads = 0;
    let (_, value) = graphemes::lower_bound(
        directory,
        17,
        fixture.source.byte_len(),
        fixture.source.utf16_len(),
        |record| {
            reads += 1;
            Ok(store.index.as_mut().unwrap().read(record)?)
        },
    )
    .unwrap();
    assert_eq!(value.unwrap().utf16, 17);
    assert!(reads < 512 && reads < directory.count / 2);
    assert!(matches!(
        graphemes::lower_bound(
            directory,
            1,
            fixture.source.byte_len(),
            fixture.source.utf16_len(),
            |record| {
                let mut row = store.index.as_mut().unwrap().read(record)?;
                if record == directory.last {
                    row[32..40].copy_from_slice(&1u64.to_le_bytes());
                }
                Ok(row)
            }
        ),
        Err(Error::InconsistentCoverage)
    ));
    store.close().unwrap();
}
