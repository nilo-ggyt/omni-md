//! 真Composer/匿名输出的行查找回归；有限oracle可保Vec，产品游标没有逐行数组。

use super::*;
use crate::{
    MarkdownDocumentBuffer, MarkdownLineComposeAnswer, MarkdownLineComposeBudget,
    MarkdownLineComposer, MarkdownLineParagraphStyle, MarkdownLineRegisteredFont,
};
#[path = "visual_caret_tests.rs"]
mod visual_caret_tests;

struct Fixture {
    _directory: tempfile::TempDir,
    document: MarkdownDocumentBuffer,
    source: MarkdownLayoutTextSource,
    catalog: MarkdownLineFontCatalog,
    font: MarkdownLineRegisteredFont,
    cancellation: MarkdownLineComposeCancellation,
}

impl Fixture {
    fn new(text: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("真实行查找.md");
        std::fs::write(&path, text).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, text.len() as u64 + 65536).unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let catalog = MarkdownLineFontCatalog::new();
        let font = crate::MarkdownLayoutFont::resolve(
            Arc::new(
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../vendor/harfrust/benches/fonts/Roboto-Regular.ttf"
                ))
                .as_slice(),
            ),
            "Roboto-Regular",
            &[],
        )
        .unwrap();
        let font = catalog.register(&font).unwrap();
        Self {
            _directory: directory,
            document,
            source,
            catalog,
            font,
            cancellation: MarkdownLineComposeCancellation::new(),
        }
    }
    fn composer(&self) -> MarkdownLineComposer {
        MarkdownLineComposer::new(
            self.source.clone(),
            &self.source.authority(),
            self.catalog.clone(),
            20 * 64,
            self.cancellation.clone(),
        )
        .unwrap()
    }
    fn next_line(&self, worker: &mut MarkdownLineComposer) -> Option<MarkdownComposedLineFacts> {
        for _ in 0..1_000_000 {
            match worker
                .step(MarkdownLineComposeBudget::default())
                .unwrap()
                .answer
            {
                MarkdownLineComposeAnswer::Yielded => {}
                MarkdownLineComposeAnswer::NeedStyle(request) => {
                    let range = request.source_range();
                    worker
                        .provide_style(
                            request
                                .respond_registered(
                                    range,
                                    &self.font,
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
                    worker
                        .provide_control(
                            request
                                .respond(
                                    &self.font,
                                    16.0,
                                    MarkdownLineParagraphStyle {
                                        tab_stops: Vec::new(),
                                        default_tab_interval_26_6: None,
                                    },
                                )
                                .unwrap(),
                        )
                        .unwrap();
                }
                MarkdownLineComposeAnswer::LineReady(facts) => return Some(facts),
                MarkdownLineComposeAnswer::Complete => return None,
                _ => panic!("真实有限夹具不应请求其他consumer"),
            }
        }
        panic!("有限真实Composer未完成");
    }
    fn output(&self) -> MarkdownComposedLines {
        let mut worker = self.composer();
        while self.next_line(&mut worker).is_some() {}
        worker.into_output().unwrap()
    }
}

fn source(at: usize, affinity: MarkdownComposedLineAffinity) -> MarkdownComposedLineLookupTarget {
    MarkdownComposedLineLookupTarget::Source {
        utf16: at,
        affinity,
    }
}

fn lookup(
    output: &mut MarkdownComposedLines,
    target: MarkdownComposedLineLookupTarget,
) -> (Option<MarkdownComposedLine>, usize) {
    let mut cursor = output.line_lookup(target).unwrap();
    let mut total = 0;
    for _ in 0..32_768 {
        let step = output.step_line_lookup(&mut cursor, 1).unwrap();
        assert!(step.records <= 1, "tail/head不能藏在一个record配额内");
        total += step.records;
        match step.answer {
            MarkdownComposedLineLookupAnswer::Pending => assert_eq!(step.records, 1),
            MarkdownComposedLineLookupAnswer::Found(line) => return (Some(line), total),
            MarkdownComposedLineLookupAnswer::PrefixNotCovered => return (None, total),
        }
    }
    panic!("真实有限跳链没有终止");
}

#[test]
fn real_lines_match_reverse_chain_oracle_with_quota_one_and_both_endpoint_sides() {
    let fixture = Fixture::new(&"a ".repeat(96));
    let mut output = fixture.output();
    let mut oracle = Vec::new();
    let mut line = output.last_line().unwrap();
    while let Some(actual) = line {
        oracle.push(actual.facts.clone());
        line = output.previous_line(&actual).unwrap();
    }
    oracle.reverse(); // 仅小夹具沿原相邻逆链建立oracle，不用新skip查找生成期望。
    assert!(oracle.len() > 32);
    for facts in &oracle {
        let (actual, _) = lookup(
            &mut output,
            MarkdownComposedLineLookupTarget::Ordinal(facts.ordinal),
        );
        assert_eq!(actual.unwrap().facts(), facts);
        let (actual, _) = lookup(
            &mut output,
            source(
                facts.source_utf16.start,
                MarkdownComposedLineAffinity::Downstream,
            ),
        );
        assert_eq!(actual.unwrap().facts(), facts);
        let (actual, _) = lookup(
            &mut output,
            source(
                facts.source_utf16.end,
                MarkdownComposedLineAffinity::Upstream,
            ),
        );
        assert_eq!(actual.unwrap().facts(), facts);
    }
    assert!(
        lookup(
            &mut output,
            MarkdownComposedLineLookupTarget::Ordinal(oracle.len())
        )
        .0
        .is_none()
    );
}

#[test]
fn deep_lookup_reads_sublinear_real_rows_instead_of_walking_every_previous_line() {
    let fixture = Fixture::new(&"a ".repeat(4096));
    let mut output = fixture.output();
    let last = output.last_line().unwrap().unwrap().facts.ordinal;
    assert!(last >= 2048);
    let (line, records) = lookup(&mut output, MarkdownComposedLineLookupTarget::Ordinal(1));
    let line = line.unwrap();
    assert_eq!(line.facts.ordinal, 1);
    assert!(
        records < 512 && records < last / 2,
        "实际rows={records}, depth={last}"
    );
    let (same, source_records) = lookup(
        &mut output,
        source(
            line.facts.source_utf16.start,
            MarkdownComposedLineAffinity::Downstream,
        ),
    );
    assert_eq!(same.unwrap().facts(), line.facts());
    assert!(source_records < 512 && source_records < last / 2);
}

#[test]
fn true_empty_and_terminal_empty_line_keep_eof_affinity_without_creating_a_next_line() {
    for text in ["", "a\n", "\n\n", "a"] {
        let fixture = Fixture::new(text);
        let mut output = fixture.output();
        let last = output.last_line().unwrap().unwrap();
        let end = fixture.source.utf16_len();
        assert_eq!(output.paragraph_base_level(&last).unwrap(), 0);
        let (down, _) = lookup(
            &mut output,
            source(end, MarkdownComposedLineAffinity::Downstream),
        );
        assert_eq!(down.unwrap().facts(), last.facts());
        let (up, _) = lookup(
            &mut output,
            source(end, MarkdownComposedLineAffinity::Upstream),
        );
        if last.facts.source_utf16.is_empty() && last.facts.ordinal > 0 {
            let previous = output.previous_line(&last).unwrap().unwrap();
            assert_eq!(up.unwrap().facts(), previous.facts());
        } else {
            assert_eq!(up.unwrap().facts(), last.facts());
        }
    }
}

#[test]
fn utf16_inside_non_bmp_scalar_only_locates_its_real_line_without_caret_authority() {
    // 原固定字体 cmap 的 format 12 真实覆盖这两个非 BMP 符号；emoji 缺字单独验证
    // typed 阻塞，不能用 .notdef 产出的行来冒充代理项查找的成功样本。
    let fixture = Fixture::new("a \u{1f16a} b \u{1f16b} c");
    let mut output = fixture.output();
    let mut line = output.last_line().unwrap();
    while let Some(actual) = line {
        for part in 0..actual.facts.parts {
            let part = output.part(&actual, part).unwrap().unwrap();
            let blank = crate::MarkdownShapedGlyph {
                glyph_id: 0,
                source_utf16_cluster: 0,
                flags: 0,
                x_advance: 0,
                y_advance: 0,
                x_offset: 0,
                y_offset: 0,
            };
            let mut batch = [blank; 256];
            assert!(part.facts().glyphs <= batch.len());
            let glyphs = part.facts().glyphs;
            output
                .read_batch(&part, 0..glyphs, &mut batch[..glyphs])
                .unwrap();
            assert!(batch[..glyphs].iter().all(|glyph| glyph.glyph_id != 0));
        }
        line = output.previous_line(&actual).unwrap();
    } // 只有原真实非零字形与原 LineEnd 都存在，才进入代理项内部的查找断言。
    for position in [2, 3, 7, 8] {
        let (actual, _) = lookup(
            &mut output,
            source(position, MarkdownComposedLineAffinity::Downstream),
        );
        let actual = actual.unwrap();
        assert!(actual.facts.source_utf16.contains(&position));
    } // 查找不会把低代理项变成source端点；返回的始终是原封口整行。
}

/// 缺字夹具仍走真实字体和字形索引，不能用换字体把原错误的 consumer 交接隐藏掉。
fn missing_font_choice(
    fixture: &Fixture,
    worker: &mut MarkdownLineComposer,
) -> crate::MarkdownSoftLineMeasureChoice {
    for _ in 0..10_000 {
        let progress = worker.step(MarkdownLineComposeBudget::default()).unwrap();
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
            MarkdownLineComposeAnswer::NeedsResolution(
                choice @ crate::MarkdownSoftLineMeasureChoice::NeedsFontFallback { .. },
            ) => return choice,
            _ => panic!("实际 emoji 缺字必须交给字体 consumer，不能完成或换用其他 consumer"),
        }
    }
    panic!("有限缺字夹具没有到达原 FontFallback 交付点");
}

#[test]
fn missing_glyph_stays_typed_without_entering_reshape_and_cancellation_releases_real_resources() {
    let fixture = Fixture::new("a 😀 b 😀 c");
    let mut worker = fixture.composer();
    let resources = worker.worker_resource_observer();
    let mapping = fixture.source.original_backing_observer().unwrap();
    let choice = missing_font_choice(&fixture, &mut worker);
    assert!(
        matches!(&choice, crate::MarkdownSoftLineMeasureChoice::NeedsFontFallback { cluster_utf16, missing_glyphs } if cluster_utf16 == &(2..4) && *missing_glyphs > 0)
    );
    let before = resources.snapshot().unwrap();
    for _ in 0..8 {
        let progress = worker.step(MarkdownLineComposeBudget::default()).unwrap();
        assert!(
            matches!(progress.answer, MarkdownLineComposeAnswer::NeedsResolution(ref actual) if actual == &choice)
        );
        assert_eq!(
            (
                progress.bytes,
                progress.scalars,
                progress.records,
                progress.metadata_units,
                progress.shapes
            ),
            (0, 0, 0, 0, 0)
        );
    } // 阻塞只重核原权，不重新整形、创建 resolver 或积累另一个缺字任务。
    let after = resources.snapshot().unwrap();
    assert_eq!(before.shapes, after.shapes);
    assert_eq!(before.clusters, after.clusters);
    assert_eq!(before.resolvers, after.resolvers);
    assert_eq!(after.resolvers.registered, 0);
    fixture.cancellation.cancel();
    assert!(matches!(
        worker.step(MarkdownLineComposeBudget::default()),
        Err(Error::Cancelled)
    ));
    let closed = resources.snapshot().unwrap();
    assert!(!closed.writer.unwrap().file_open && closed.writer.unwrap().mapped_bytes == 0);
    assert_eq!(closed.shapes.active, 0);
    drop((worker, fixture));
    assert!(resources.is_released());
    assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
}

#[test]
fn original_source_error_precedes_cancellation_even_while_font_consumer_is_pending() {
    let fixture = Fixture::new("a 😀 b");
    let mut worker = fixture.composer();
    let resources = worker.worker_resource_observer();
    missing_font_choice(&fixture, &mut worker);
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
    fixture.cancellation.cancel();
    for _ in 0..2 {
        assert!(matches!(
            worker.step(MarkdownLineComposeBudget::default()),
            Err(Error::Source(
                crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
            ))
        ));
    } // 原失败保持 sticky；撤销后的第二次调用也不能把实际 EIO 改写成普通取消或缺字。
    let closed = resources.snapshot().unwrap();
    assert!(!closed.writer.unwrap().file_open && closed.writer.unwrap().mapped_bytes == 0);
    assert_eq!(closed.resolvers.registered, 0);
    assert_eq!(closed.shapes.active, 0);
}

#[cfg(feature = "paged-shaping-frontier")]
#[test]
fn old_prefix_downstream_end_is_not_eof_and_foreign_snapshot_cannot_resume_cursor() {
    let fixture = Fixture::new(&"a ".repeat(80));
    let mut worker = fixture.composer();
    let first = fixture.next_line(&mut worker).unwrap();
    let mut old = worker.publish_ready_prefix().unwrap().unwrap();
    let mut old_cursor = old
        .line_lookup(MarkdownComposedLineLookupTarget::Ordinal(0))
        .unwrap();
    fixture.next_line(&mut worker).unwrap();
    let mut new = worker.publish_ready_prefix().unwrap().unwrap();
    assert!(
        lookup(
            &mut old,
            source(
                first.source_utf16.end,
                MarkdownComposedLineAffinity::Downstream
            )
        )
        .0
        .is_none()
    );
    assert_eq!(
        lookup(
            &mut old,
            source(
                first.source_utf16.end,
                MarkdownComposedLineAffinity::Upstream
            )
        )
        .0
        .unwrap()
        .facts(),
        &first
    );
    let (following, _) = lookup(
        &mut new,
        source(
            first.source_utf16.end,
            MarkdownComposedLineAffinity::Downstream,
        ),
    );
    assert_eq!(following.unwrap().facts.ordinal, 1);
    assert!(matches!(
        new.step_line_lookup(&mut old_cursor, 1),
        Err(Error::SourceMismatch)
    ));
    assert!(!new.resource_observer().snapshot().file_open);
    assert_eq!(old.last_line().unwrap().unwrap().facts(), &first);
}

/// 仅破坏小夹具派生记录的独立匿名副本；原Source和正文始终未改。每次只读/写一条64B，
/// 最终公开游标实际pread到损坏header，不用直接调私有decode假装覆盖了I/O消费者。
fn corrupted(
    output: &mut MarkdownComposedLines,
    mutate: impl FnOnce(&mut [u8; 64]),
) -> MarkdownComposedLines {
    let last = output.last_line().unwrap().unwrap();
    let mut mutation = Some(mutate);
    let mut index = PagedLayoutIndex::<64>::new().unwrap();
    let count = output.payload.as_ref().unwrap().reader.len();
    for record in 0..count {
        let mut row = output.row(record).unwrap();
        if record == last.header {
            mutation.take().unwrap()(&mut row);
        }
        index.append(row).unwrap();
    }
    let payload = output.payload.as_ref().unwrap();
    let identity = Arc::new(());
    MarkdownComposedLines::from_sealed(OutputSeal {
        source: payload.source.clone(),
        authority: output.authority.clone(),
        fonts: payload.fonts.clone(),
        cancellation: output.cancellation.clone(),
        reader: index.into_reader().unwrap(),
        #[cfg(feature = "paged-shaping-frontier")]
        lineage: Arc::downgrade(&identity),
        identity,
        last: output.last,
        count: output.count,
        range: output.range.clone(),
        graphemes: output.graphemes,
        folds: payload.folds.clone(),
        #[cfg(all(
            feature = "s2-table-window-prototype",
            feature = "paged-shaping-frontier"
        ))]
        tables: payload.tables.clone(),
    })
    .unwrap()
}

#[test]
fn malformed_skip_ordinal_record_and_nonmonotone_source_permanently_reject_real_reader() {
    let fixture = Fixture::new(&"a ".repeat(16));
    let mut output = fixture.output();
    for field in [40, 48, 0] {
        let mut damaged = corrupted(&mut output, |row| {
            let value = if field == 0 {
                u64_at(row, 8) + 1
            } else {
                u64::MAX - 1
            };
            row[field..field + 8].copy_from_slice(&value.to_le_bytes());
        });
        let receipt = damaged.resource_observer();
        let mut cursor = damaged
            .line_lookup(MarkdownComposedLineLookupTarget::Ordinal(0))
            .unwrap();
        assert_eq!(damaged.step_line_lookup(&mut cursor, 1).unwrap().records, 1);
        assert!(matches!(
            damaged.step_line_lookup(&mut cursor, 1),
            Err(Error::InconsistentCoverage)
        ));
        assert!(!receipt.snapshot().file_open && receipt.snapshot().mapped_bytes == 0);
        assert!(matches!(
            damaged.step_line_lookup(&mut cursor, 1),
            Err(Error::Poisoned)
        ));
    }
    output.last_line().unwrap(); // 损坏副本失败不能撤销原健康Output。
}

#[test]
fn source_error_precedes_cancel_and_cached_tail_with_actual_close_and_unmap() {
    let fixture = Fixture::new(&"a ".repeat(32));
    let source_receipt = fixture.source.resource_observer();
    let backing = fixture.source.original_backing_observer().unwrap();
    let mut output = fixture.output();
    let receipt = output.resource_observer();
    let mut cursor = output
        .line_lookup(MarkdownComposedLineLookupTarget::Ordinal(0))
        .unwrap();
    assert_eq!(output.step_line_lookup(&mut cursor, 1).unwrap().records, 1);
    fixture.document.fail_next_original_reclaim_for_test();
    assert_eq!(
        fixture.document.reclaim_original_read_windows_for_test(),
        Err(crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO))
    );
    fixture.cancellation.cancel();
    assert!(matches!(output.step_line_lookup(&mut cursor, 1),
        Err(Error::Source(crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(errno))) if errno == libc::EIO));
    assert!(!receipt.snapshot().file_open && receipt.snapshot().mapped_bytes == 0);
    drop(output);
    drop(fixture);
    assert!(source_receipt.payload_released());
    assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0);
    assert!(backing.failed() && backing.first_errno() == Some(libc::EIO));
    drop(cursor); // 游标尚活时Source/FD已实际归还；弱票不能成为业务强边。
}

#[test]
fn cancelled_cursor_cannot_resume_and_healthy_cursor_does_not_keep_output_resources_alive() {
    let fixture = Fixture::new("a a a a");
    let mut output = fixture.output();
    let observer = output.resource_observer();
    let mut cursor = output
        .line_lookup(MarkdownComposedLineLookupTarget::Ordinal(0))
        .unwrap();
    fixture.cancellation.cancel();
    assert!(matches!(
        output.step_line_lookup(&mut cursor, 1),
        Err(Error::Cancelled)
    ));
    assert!(!observer.snapshot().file_open);
    let other = Fixture::new("a a a a");
    let mut output = other.output();
    let source = other.source.resource_observer();
    let backing = other.source.original_backing_observer().unwrap();
    let cursor = output
        .line_lookup(MarkdownComposedLineLookupTarget::Ordinal(0))
        .unwrap();
    output.close().unwrap();
    drop(other);
    assert!(source.payload_released() && backing.unmap_succeeded());
    drop(cursor);
}

#[test]
fn identical_bytes_in_other_source_cannot_receive_a_line_from_foreign_lookup() {
    let first = Fixture::new("a a a a");
    let second = Fixture::new("a a a a");
    let mut original = first.output();
    let mut other = second.output();
    let mut cursor = original
        .line_lookup(MarkdownComposedLineLookupTarget::Ordinal(0))
        .unwrap();
    let receipt = other.resource_observer();
    assert!(matches!(
        other.step_line_lookup(&mut cursor, 1),
        Err(Error::SourceMismatch)
    ));
    assert!(!receipt.snapshot().file_open);
    original.last_line().unwrap().unwrap(); // 错误只归还接收方，不撤销仍健康的原Output。
}

#[test]
fn invalid_record_quota_rejects_before_reading_and_does_not_revive_the_reader() {
    for quota in [0, STEP + 1] {
        let fixture = Fixture::new("a a a a");
        let mut output = fixture.output();
        let receipt = output.resource_observer();
        let mut cursor = output
            .line_lookup(MarkdownComposedLineLookupTarget::Ordinal(0))
            .unwrap();
        assert!(matches!(
            output.step_line_lookup(&mut cursor, quota),
            Err(Error::InvalidRequest)
        ));
        assert!(cursor.tail.is_none() && cursor.current.is_none());
        assert!(!receipt.snapshot().file_open && receipt.snapshot().mapped_bytes == 0);
        assert!(matches!(
            output.step_line_lookup(&mut cursor, 1),
            Err(Error::Poisoned)
        ));
    }
}
