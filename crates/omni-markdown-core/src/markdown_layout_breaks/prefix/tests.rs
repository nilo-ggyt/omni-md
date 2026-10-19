//! 原LB的稳定前缀、严格EOF和有限调度需求；原机器仍是唯一规则实现。
use super::*;

fn fixture(
    text: &str,
) -> (
    tempfile::TempDir,
    crate::MarkdownDocumentBuffer,
    MarkdownLayoutTextSource,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("断行稳定前缀.md");
    std::fs::write(&path, text).unwrap();
    let document = crate::MarkdownDocumentBuffer::open(&path, text.len() as u64 + 65536).unwrap();
    let source = MarkdownLayoutTextSource::capture(&document);
    (directory, document, source)
}

fn one() -> MarkdownLineBreakBudget {
    MarkdownLineBreakBudget {
        bytes: 1,
        scalars: 1,
        records: 1,
    }
}

#[test]
fn only_actual_whole_source_dense_gcb_can_relax_line_eof_and_frontier_is_strict() {
    for text in ["abcde", "", "a\n", "a🙂", "a\u{ad}"] {
        let (_directory, _document, source) = fixture(text);
        let authority = source.authority();
        let mut job = MarkdownLayoutBreakJob::new(source, &authority).unwrap();
        assert_eq!(
            job.ascii_prefix_ready(&authority).unwrap(),
            matches!(text, "abcde" | "")
        );
        assert!(!job.payload.as_ref().unwrap().line.is_ready());
        assert_eq!(job.line_source_bytes_read_for_test(), 0);
    }
    let (_directory, _document, source) = fixture("abcde");
    let authority = source.authority();
    let mut job = MarkdownLayoutBreakJob::new(source, &authority).unwrap();
    for _ in 0..3 {
        let step = job.step(&authority, one()).unwrap();
        assert_eq!((step.bytes, step.scalars, step.records), (1, 1, 0));
        assert!(!step.ready);
    }
    // 三个真实ASCII单元后最早待决位置是1；位置1本身没有“非机会”权限。
    let mut at = job
        .cursor(
            MarkdownLayoutBreakUnit::Utf16,
            1,
            MarkdownLayoutBreakQuery::At,
        )
        .unwrap();
    assert_eq!(
        job.query(&mut at, 1).unwrap(),
        MarkdownLayoutBreakAnswer::PendingSource
    );
    assert!(job.take_line_source_demand(&authority).unwrap());
    assert!(!job.take_line_source_demand(&authority).unwrap());
    job.step(&authority, one()).unwrap();
    assert_eq!(
        job.query(&mut at, 1).unwrap(),
        MarkdownLayoutBreakAnswer::Known(None)
    );
    assert!(!job.take_line_source_demand(&authority).unwrap());
}

#[test]
fn physical_eof_without_final_flush_is_pending_and_never_known_none() {
    let (_directory, _document, source) = fixture("a b");
    let authority = source.authority();
    let mut job = MarkdownLayoutBreakJob::new(source, &authority).unwrap();
    let step = job
        .step(
            &authority,
            MarkdownLineBreakBudget {
                bytes: 3,
                scalars: 3,
                records: 1,
            },
        )
        .unwrap();
    assert_eq!(step.bytes, 3);
    assert!(!step.ready); // 唯一records余额写实际空格机会，真实末尾Mandatory尚未写入。
    let mut at = job
        .cursor(
            MarkdownLayoutBreakUnit::Utf16,
            3,
            MarkdownLayoutBreakQuery::At,
        )
        .unwrap();
    assert_eq!(
        job.query(&mut at, 1).unwrap(),
        MarkdownLayoutBreakAnswer::PendingSource
    );
    assert!(job.take_line_source_demand(&authority).unwrap());
    let step = job.step(&authority, one()).unwrap();
    assert_eq!(
        (step.bytes, step.scalars, step.records, step.ready),
        (0, 0, 1, true)
    );
    let MarkdownLayoutBreakAnswer::Known(Some(end)) = job.query(&mut at, 1).unwrap() else {
        panic!("真实EOF应有Mandatory机会")
    };
    assert_eq!(
        (end.byte, end.utf16, end.kind),
        (3, 3, crate::MarkdownLineBreakKind::Mandatory)
    );
    let mut next = job
        .cursor(
            MarkdownLayoutBreakUnit::Utf16,
            3,
            MarkdownLayoutBreakQuery::Next,
        )
        .unwrap();
    assert_eq!(
        job.query(&mut next, 1).unwrap(),
        MarkdownLayoutBreakAnswer::Known(None)
    );
    assert!(!job.take_line_source_demand(&authority).unwrap());
}

#[test]
fn pending_gcb_and_candidate_yield_do_not_request_line_source() {
    let (_directory, _document, source) = fixture("a b🙂c");
    let authority = source.authority();
    let mut job = MarkdownLayoutBreakJob::new(source, &authority).unwrap();
    while !job.payload.as_ref().unwrap().line.is_ready() {
        job.payload
            .as_mut()
            .unwrap()
            .line
            .step(&authority, MarkdownLineBreakBudget::default())
            .unwrap();
    }
    let mut next = job
        .cursor(
            MarkdownLayoutBreakUnit::Utf16,
            0,
            MarkdownLayoutBreakQuery::Next,
        )
        .unwrap();
    assert_eq!(
        job.query(&mut next, 1).unwrap(),
        MarkdownLayoutBreakAnswer::PendingSource
    );
    assert!(!job.take_line_source_demand(&authority).unwrap());
    // 字素未完成不能进入ASCII前缀门，也不能把这次等待错派成已完成LB的正文读取。
    assert!(!job.ascii_prefix_ready(&authority).unwrap());

    let (_directory, _document, source) = fixture(" \u{301} \u{301}");
    let authority = source.authority();
    let mut job = MarkdownLayoutBreakJob::new(source, &authority).unwrap();
    while !job.step(&authority, one()).unwrap().ready {}
    let mut next = job
        .cursor(
            MarkdownLayoutBreakUnit::Utf16,
            0,
            MarkdownLayoutBreakQuery::Next,
        )
        .unwrap();
    assert_eq!(
        job.query(&mut next, 1).unwrap(),
        MarkdownLayoutBreakAnswer::Yielded
    );
    assert!(!job.take_line_source_demand(&authority).unwrap());
}

#[test]
fn long_space_and_open_punctuation_keep_real_pending_instead_of_inventing_a_prefix_break() {
    for text in [
        format!("{}a", " ".repeat(4096)),
        format!("({}a", " ".repeat(4096)),
    ] {
        let (_directory, _document, source) = fixture(&text);
        let authority = source.authority();
        let mut job = MarkdownLayoutBreakJob::new(source, &authority).unwrap();
        let mut next = job
            .cursor(
                MarkdownLayoutBreakUnit::Utf16,
                0,
                MarkdownLayoutBreakQuery::Next,
            )
            .unwrap();
        for _ in 0..1024 {
            let step = job.step(&authority, one()).unwrap();
            assert!(step.bytes <= 1 && step.scalars <= 1 && step.records <= 1 && !step.ready);
        }
        assert_eq!(
            job.query(&mut next, 1).unwrap(),
            MarkdownLayoutBreakAnswer::PendingSource
        );
        assert!(job.take_line_source_demand(&authority).unwrap());
        assert_eq!(job.line_source_bytes_read_for_test(), 1024);
        while !job.step(&authority, one()).unwrap().ready {}
        assert!(matches!(
            job.query(&mut next, 1).unwrap(),
            MarkdownLayoutBreakAnswer::Known(Some(_))
        ));
    }
}

#[test]
fn quota_one_prefix_queries_match_independent_complete_raw_line_job() {
    let (_directory, _document, source) = fixture("one (two) 12,345.67 USD / next [word] end");
    let authority = source.authority();
    let mut oracle = MarkdownLineBreakJob::new(source.clone()).unwrap();
    while !oracle.is_ready() {
        oracle
            .step(&authority, MarkdownLineBreakBudget::default())
            .unwrap();
    }
    let mut actual = MarkdownLayoutBreakJob::new(source.clone(), &authority).unwrap();
    for offset in 0..=source.utf16_len() {
        let expected = oracle
            .query(
                &authority,
                MarkdownLineBreakUnit::Utf16,
                offset,
                MarkdownLineBreakQuery::IsBoundary,
            )
            .unwrap();
        let mut cursor = actual
            .cursor(
                MarkdownLayoutBreakUnit::Utf16,
                offset,
                MarkdownLayoutBreakQuery::At,
            )
            .unwrap();
        loop {
            match actual.query(&mut cursor, 1).unwrap() {
                MarkdownLayoutBreakAnswer::PendingSource => {
                    assert!(actual.take_line_source_demand(&authority).unwrap());
                    let step = actual.step(&authority, one()).unwrap();
                    assert!(step.bytes <= 1 && step.scalars <= 1 && step.records <= 1);
                }
                MarkdownLayoutBreakAnswer::Known(value) => {
                    assert_eq!(LineAnswer::Known(value), expected);
                    break;
                }
                MarkdownLayoutBreakAnswer::Yielded => panic!("单点机会无需跳过GCB"),
            }
        }
    }
    assert!(actual.payload.as_ref().unwrap().line.is_ready());
}

#[test]
fn source_failure_precedes_pending_demand_and_cancel_releases_actual_index_owners() {
    let (_directory, document, source) = fixture("a b c d");
    let authority = source.authority();
    let backing = source.original_backing_observer().unwrap();
    let source_observer = source.resource_observer();
    let mut job = MarkdownLayoutBreakJob::new(source.clone(), &authority).unwrap();
    let resources = job.resource_observer();
    job.step(&authority, one()).unwrap(); // 先真实读取并驻留原页，故障必须作用于真实归还。
    document.fail_next_original_reclaim_for_test();
    assert!(matches!(
        document.reclaim_original_read_windows_for_test(),
        Err(crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO))
    ));
    assert!(matches!(
        job.take_line_source_demand(&authority),
        Err(MarkdownLayoutBreakError::Source(
            crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
    job.cancel();
    let (grapheme, line) = resources.snapshots();
    assert!(
        grapheme.metadata_owner_released
            && !line.metadata_file_open
            && line.metadata_map_bytes == 0
            && !line.cleanup_failed
    );
    drop((job, source, document));
    assert!(source_observer.payload_released());
    assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0);
}

#[test]
fn foreign_source_cannot_enable_dense_prefix_and_revokes_real_owner() {
    let (_directory, _document, source) = fixture("same bytes");
    let (_other_directory, _other_document, other) = fixture("same bytes");
    let mut job = MarkdownLayoutBreakJob::new(source.clone(), &source.authority()).unwrap();
    let resources = job.resource_observer();
    assert!(matches!(
        job.ascii_prefix_ready(&other.authority()),
        Err(MarkdownLayoutBreakError::SourceMismatch)
    ));
    let (grapheme, line) = resources.snapshots();
    assert!(
        grapheme.metadata_owner_released
            && !line.metadata_file_open
            && line.metadata_map_bytes == 0
            && !line.cleanup_failed
    );
    assert!(matches!(
        job.ascii_prefix_ready(&source.authority()),
        Err(MarkdownLayoutBreakError::Poisoned)
    ));
}

#[test]
fn unicode_grapheme_pending_advances_only_original_gcb_and_preserves_line_demand_separately() {
    let (_directory, _document, source) = fixture("中𠮷A\u{0301}文");
    let authority = source.authority();
    let mut job = MarkdownLayoutBreakJob::new(source.clone(), &authority).unwrap();
    assert_eq!(
        job.grapheme_query(&authority, 0, MarkdownGraphemeQuery::Next)
            .unwrap(),
        GraphemeAnswer::Pending
    );
    assert!(job.take_grapheme_source_demand(&authority).unwrap());
    assert!(!job.take_line_source_demand(&authority).unwrap());
    let mut first = None;
    for _ in 0..64 {
        let step = job.step_dependency(&authority, true, one()).unwrap();
        assert!(step.bytes <= 1 && step.scalars <= 1 && step.records <= 1);
        assert_eq!(job.line_source_bytes_read_for_test(), 0);
        match job
            .grapheme_query(&authority, 0, MarkdownGraphemeQuery::Next)
            .unwrap()
        {
            GraphemeAnswer::Known(Some(value)) => {
                first = Some(value);
                break;
            }
            GraphemeAnswer::Pending => {
                assert!(job.take_grapheme_source_demand(&authority).unwrap());
            }
            _ => panic!("原真实下一GCB不能消失"),
        }
    }
    assert_eq!(
        first,
        Some(crate::MarkdownGraphemeBoundary { byte: 3, utf16: 1 })
    );
    let prefix = job.grapheme_prefix_facts(&authority).unwrap();
    assert!(!prefix.complete && prefix.total_count.is_none());
    let mut cursor = job
        .cursor(
            MarkdownLayoutBreakUnit::Utf16,
            0,
            MarkdownLayoutBreakQuery::Next,
        )
        .unwrap();
    assert_eq!(
        job.query(&mut cursor, 1).unwrap(),
        MarkdownLayoutBreakAnswer::PendingSource
    );
    assert!(job.take_line_source_demand(&authority).unwrap());
    let before = job.grapheme_prefix_facts(&authority).unwrap();
    let step = job.step_dependency(&authority, false, one()).unwrap();
    assert_eq!(step.bytes, 1);
    assert_eq!(job.grapheme_prefix_facts(&authority).unwrap(), before);
}

#[test]
fn partial_ordinal_reads_stay_pending_until_original_records_exist_and_do_not_mutate_batch() {
    let (_directory, _document, source) = fixture("中𠮷文");
    let authority = source.authority();
    let mut job = MarkdownLayoutBreakJob::new(source, &authority).unwrap();
    let marker = crate::MarkdownGraphemeBoundary {
        byte: 99,
        utf16: 88,
    };
    let mut batch = [marker; 2];
    assert!(
        !job.read_grapheme_prefix(&authority, 0..2, &mut batch)
            .unwrap()
    );
    assert_eq!(batch, [marker; 2]);
    assert!(job.take_grapheme_source_demand(&authority).unwrap());
    while job
        .grapheme_prefix_facts(&authority)
        .unwrap()
        .known_boundary_count
        < 2
    {
        job.step_dependency(&authority, true, one()).unwrap();
    }
    assert!(
        job.read_grapheme_prefix(&authority, 0..2, &mut batch)
            .unwrap()
    );
    assert_eq!(
        batch,
        [
            crate::MarkdownGraphemeBoundary { byte: 0, utf16: 0 },
            crate::MarkdownGraphemeBoundary { byte: 3, utf16: 1 }
        ]
    );
    assert!(!job.grapheme_prefix_facts(&authority).unwrap().complete);
    assert_eq!(job.line_source_bytes_read_for_test(), 0);
}

#[test]
fn certified_endpoints_require_same_original_scope_and_do_not_keep_source_or_indices_alive() {
    let (directory, document, source) = fixture("中𠮷");
    let authority = source.authority();
    let backing = source.original_backing_observer().unwrap();
    let observed = source.resource_observer();
    let mut first = MarkdownLayoutBreakJob::new(source.clone(), &authority).unwrap();
    let mut second = MarkdownLayoutBreakJob::new(source.clone(), &authority).unwrap();
    for job in [&mut first, &mut second] {
        while !job
            .step(&authority, MarkdownLineBreakBudget::default())
            .unwrap()
            .ready
        {}
    }
    let (_, start) = first.certified_grapheme_endpoint(&authority, 0).unwrap();
    let (_, end) = first.certified_grapheme_endpoint(&authority, 3).unwrap();
    let (_, foreign_end) = second.certified_grapheme_endpoint(&authority, 3).unwrap();
    let start = start.unwrap();
    let end = end.unwrap();
    let foreign_end = foreign_end.unwrap();
    assert!(start.same_scope(&end));
    assert!(!start.same_scope(&foreign_end));
    assert_eq!(
        end.boundary(&authority).unwrap(),
        crate::MarkdownGraphemeBoundary { byte: 7, utf16: 3 }
    );
    first.cancel();
    assert!(matches!(
        end.boundary(&authority),
        Err(MarkdownLayoutBreakError::Poisoned)
    ));
    drop((first, second, source, document, directory));
    assert!(observed.payload_released());
    assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
    // 三张端点票仍活，但只持纯坐标/弱scope，不保活原Source或任一索引。
    assert!(!start.same_scope(&foreign_end));
}

#[test]
fn certified_gb2_on_four_mib_crlf_source_does_not_scan_or_publish_final_ordinals() {
    use std::io::Write;

    // 真实4MiB文件按固定块写入；每块含CRLF，使原GCB确实不能走全源Printable Dense。
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("四MiB真实CRLF末端.md");
    let mut file = std::fs::File::create(&path).unwrap();
    let mut block = [b'x'; 4096];
    block[..2].copy_from_slice(b"\r\n");
    for _ in 0..1024 {
        file.write_all(&block).unwrap();
    }
    drop(file);
    let document = crate::MarkdownDocumentBuffer::open(&path, 4 * 1024 * 1024 + 65536).unwrap();
    let source = MarkdownLayoutTextSource::capture(&document);
    let authority = source.authority();
    let backing = source.original_backing_observer().unwrap();
    let observed = source.resource_observer();
    let mut job = MarkdownLayoutBreakJob::new(source.clone(), &authority).unwrap();
    let resources = job.resource_observer();
    let before = job.payload.as_ref().unwrap().grapheme.storage_profile();
    let facts = job.grapheme_prefix_facts(&authority).unwrap();
    assert!(!before.dense_printable_ascii && !facts.complete && facts.total_count.is_none());
    assert_eq!(before.source_bytes_read, 0);
    assert_eq!(
        (before.source_bytes, before.source_utf16),
        (4 * 1024 * 1024, 4 * 1024 * 1024)
    );
    assert!(!job.take_grapheme_source_demand(&authority).unwrap());

    let expected = crate::MarkdownGraphemeBoundary {
        byte: 4 * 1024 * 1024,
        utf16: 4 * 1024 * 1024,
    };
    let (answer, end) = job
        .certified_grapheme_endpoint(&authority, expected.utf16)
        .unwrap();
    let end = end.unwrap();
    assert_eq!(answer, GraphemeAnswer::Known(Some(expected)));
    assert_eq!(end.boundary(&authority).unwrap(), expected);
    assert!(!job.take_grapheme_source_demand(&authority).unwrap());
    assert_eq!(job.grapheme_prefix_facts(&authority).unwrap(), facts);
    assert_eq!(job.ready_grapheme_count(&authority).unwrap(), None);

    // GB2只确认坐标：原未知EOF分类、前一字素和ordinal读取仍必须等待真实GCB记录。
    for query in [
        MarkdownGraphemeQuery::IsBoundary,
        MarkdownGraphemeQuery::Previous,
    ] {
        assert_eq!(
            job.grapheme_query(&authority, expected.utf16, query)
                .unwrap(),
            GraphemeAnswer::Pending
        );
    }
    let marker = crate::MarkdownGraphemeBoundary {
        byte: usize::MAX,
        utf16: usize::MAX,
    };
    let mut output = [marker];
    assert!(
        !job.read_grapheme_prefix(&authority, 0..1, &mut output)
            .unwrap()
    );
    assert_eq!(output, [marker]);
    let (_, repeated) = job
        .certified_grapheme_endpoint(&authority, expected.utf16)
        .unwrap();
    assert!(end.same_scope(&repeated.unwrap()));
    assert!(job.take_grapheme_source_demand(&authority).unwrap());
    assert!(!job.take_line_source_demand(&authority).unwrap());

    // 原扫描、索引行和映射的真实计量都不变，不能只凭返回值或新计数宣称避免了读取。
    let after = job.payload.as_ref().unwrap().grapheme.storage_profile();
    assert_eq!(after.source_bytes_read, before.source_bytes_read);
    assert_eq!(
        after.source_borrow_max_bytes,
        before.source_borrow_max_bytes
    );
    assert_eq!(after.boundary_count, before.boundary_count);
    assert_eq!(after.boundary_records, before.boundary_records);
    assert_eq!(after.index_records_read, before.index_records_read);
    assert_eq!(after.index_records_written, before.index_records_written);
    assert_eq!(after.index_page_loads, before.index_page_loads);
    assert_eq!(
        after.index_peak_mapped_bytes,
        before.index_peak_mapped_bytes
    );
    assert_eq!(job.line_source_bytes_read_for_test(), 0);
    drop((job, source, document));
    let (grapheme, line) = resources.snapshots();
    assert!(grapheme.metadata_owner_released && !grapheme.cleanup_failed);
    assert!(!line.metadata_file_open && line.metadata_map_bytes == 0 && !line.cleanup_failed);
    assert!(observed.payload_released());
    assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
    assert!(matches!(
        end.boundary(&authority),
        Err(MarkdownLayoutBreakError::Poisoned)
    ));
}

#[test]
fn gb2_before_indexing_keeps_original_scope_and_rejects_another_capture() {
    let (_directory, document, source) = fixture("中\r\n𠮷");
    let authority = source.authority();
    let other_capture = MarkdownLayoutTextSource::capture(&document);
    let mut first = MarkdownLayoutBreakJob::new(source.clone(), &authority).unwrap();
    let mut second = MarkdownLayoutBreakJob::new(source, &authority).unwrap();
    let (_, first_end) = first.certified_grapheme_endpoint(&authority, 5).unwrap();
    let (_, second_end) = second.certified_grapheme_endpoint(&authority, 5).unwrap();
    let first_end = first_end.unwrap();
    let second_end = second_end.unwrap();
    assert!(!first_end.same_scope(&second_end));
    assert_eq!(first.ready_grapheme_count(&authority).unwrap(), None);

    // 同Doc、同代次、同长度的重新capture仍没有原任务权限；失败撤掉此前GB2票的弱scope。
    assert!(matches!(
        first.certified_grapheme_endpoint(&other_capture.authority(), 5),
        Err(MarkdownLayoutBreakError::SourceMismatch)
    ));
    assert!(matches!(
        first_end.boundary(&authority),
        Err(MarkdownLayoutBreakError::Poisoned)
    ));
    assert_eq!(
        second_end.boundary(&authority).unwrap(),
        crate::MarkdownGraphemeBoundary { byte: 9, utf16: 5 }
    );
}

#[test]
fn gb2_rejects_cancelled_original_grapheme_and_releases_the_original_scope() {
    for child_only in [false, true] {
        let (_directory, _document, source) = fixture("中\r\n");
        let authority = source.authority();
        let mut job = MarkdownLayoutBreakJob::new(source, &authority).unwrap();
        let resources = job.resource_observer();
        let (_, end) = job.certified_grapheme_endpoint(&authority, 3).unwrap();
        let end = end.unwrap();
        if child_only {
            // 子算法取消尚未让父payload退场；GB2必须走原子算法健康门并使父任务真实失败关闭。
            job.payload.as_mut().unwrap().grapheme.cancel();
            assert!(matches!(
                job.certified_grapheme_endpoint(&authority, 3),
                Err(MarkdownLayoutBreakError::Grapheme(
                    crate::MarkdownGraphemeError::Poisoned
                ))
            ));
        } else {
            // 父任务取消直接归还原payload；已知末端长度也不能在此后签出新票。
            job.cancel();
            assert!(matches!(
                job.certified_grapheme_endpoint(&authority, 3),
                Err(MarkdownLayoutBreakError::Poisoned)
            ));
        }
        assert!(matches!(
            end.boundary(&authority),
            Err(MarkdownLayoutBreakError::Poisoned)
        ));
        let (grapheme, line) = resources.snapshots();
        assert!(grapheme.metadata_owner_released && !grapheme.cleanup_failed);
        assert!(!line.metadata_file_open && line.metadata_map_bytes == 0 && !line.cleanup_failed);
    }
}

#[test]
fn gb2_preserves_real_source_eio_before_foreign_capture_or_child_cancellation() {
    let (_directory, document, source) = fixture("中\r\n𠮷");
    let authority = source.authority();
    let foreign = MarkdownLayoutTextSource::capture(&document).authority();
    let mut job = MarkdownLayoutBreakJob::new(source, &authority).unwrap();
    let resources = job.resource_observer();
    let (_, end) = job.certified_grapheme_endpoint(&authority, 5).unwrap();
    let end = end.unwrap();
    job.payload.as_mut().unwrap().grapheme.cancel();
    // 先建立真实原文读窗，随后注入其实际归还失败；不伪造字素或observer的错误状态。
    document
        .text_snapshot()
        .visit_byte_range(0..1, |_| Ok(()))
        .unwrap();
    document.fail_next_original_reclaim_for_test();
    assert_eq!(
        document.reclaim_original_read_windows_for_test(),
        Err(crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO))
    );
    assert!(matches!(
        job.certified_grapheme_endpoint(&foreign, 5),
        Err(MarkdownLayoutBreakError::Source(
            crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
    assert!(matches!(
        end.boundary(&authority),
        Err(MarkdownLayoutBreakError::Poisoned)
    ));
    let (grapheme, line) = resources.snapshots();
    assert!(grapheme.metadata_owner_released && !grapheme.cleanup_failed);
    assert!(!line.metadata_file_open && line.metadata_map_bytes == 0 && !line.cleanup_failed);
}

#[test]
fn gb2_does_not_certify_low_surrogates_or_crlf_interior_and_empty_source_stays_exact() {
    let (_directory, _document, source) = fixture("中𠮷\r\n");
    let authority = source.authority();
    let mut job = MarkdownLayoutBreakJob::new(source, &authority).unwrap();
    for offset in [2, 4] {
        let (answer, endpoint) = job.certified_grapheme_endpoint(&authority, offset).unwrap();
        assert_eq!(answer, GraphemeAnswer::Pending);
        assert!(endpoint.is_none());
    }
    let (_, end) = job.certified_grapheme_endpoint(&authority, 5).unwrap();
    assert_eq!(
        end.unwrap().boundary(&authority).unwrap(),
        crate::MarkdownGraphemeBoundary { byte: 9, utf16: 5 }
    );
    // 内部位置只能等原GCB真实完成后分类；EOF权限不能把低代理项或CRLF中点升级成边界。
    while !job.payload.as_ref().unwrap().grapheme.is_ready() {
        job.step_dependency(&authority, true, one()).unwrap();
    }
    for offset in [2, 4] {
        let (answer, endpoint) = job.certified_grapheme_endpoint(&authority, offset).unwrap();
        assert_eq!(answer, GraphemeAnswer::Known(None));
        assert!(endpoint.is_none());
    }

    // 空Source的0是真实GB2，保持原Dense完成/计数事实，不能凭新路径改变原任务状态。
    let (_directory, _document, source) = fixture("");
    let authority = source.authority();
    let mut job = MarkdownLayoutBreakJob::new(source, &authority).unwrap();
    let facts = job.grapheme_prefix_facts(&authority).unwrap();
    let (answer, end) = job.certified_grapheme_endpoint(&authority, 0).unwrap();
    let expected = crate::MarkdownGraphemeBoundary { byte: 0, utf16: 0 };
    assert_eq!(answer, GraphemeAnswer::Known(Some(expected)));
    assert_eq!(end.unwrap().boundary(&authority).unwrap(), expected);
    assert_eq!(job.grapheme_prefix_facts(&authority).unwrap(), facts);
    assert!(!job.take_grapheme_source_demand(&authority).unwrap());
}
