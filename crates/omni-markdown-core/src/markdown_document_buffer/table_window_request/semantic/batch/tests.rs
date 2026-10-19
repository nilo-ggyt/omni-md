use super::*;
use crate::markdown_viewport_presentation::MarkdownViewportPresentation;

fn document(source: &str) -> (tempfile::TempDir, MarkdownDocumentBuffer) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("多表窗口.md");
    std::fs::write(&path, source).unwrap();
    let doc = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap();
    (directory, doc)
}
fn finish(
    request: &mut MarkdownTableViewportRequest,
    doc: &MarkdownDocumentBuffer,
) -> Result<(), MarkdownTableRequestError> {
    for _ in 0..doc.byte_len().saturating_mul(64).saturating_add(10000) {
        match request.advance(doc, false, 4096)? {
            MarkdownTableViewportStep::Ready { .. } => return Ok(()),
            MarkdownTableViewportStep::Progress { work } => {
                assert!(work.source_bytes <= 4096 && work.structural_transitions <= 4096);
            }
        }
    }
    panic!("有限真实夹具未完成");
}
fn units(source: &str, bytes: usize) -> usize {
    source[..bytes].encode_utf16().count()
}

#[test]
fn multi_table_batch_preserves_source_partition_and_shared_first_wins_semantics() {
    let source = "[a]: /first\r\n\r\n普通段落\r\n\r\n> | H | C |\r\n> | - | - |\r\n> | [a] | **一** |\r\n\r\n中间 **正文**\r\n\r\n| X |\r\n| - |\r\n| [b] |\r\n\r\n[b]: /later\r\n[a]: /wrong\r\n";
    let (_directory, doc) = document(source);
    let owners = Arc::strong_count(doc.root.as_ref().unwrap());
    let viewport = 0..doc.utf16_len();
    let mut request = doc
        .table_viewport_request(viewport.clone(), 31, 0..0)
        .unwrap();
    finish(&mut request, &doc).unwrap();
    assert_eq!(Arc::strong_count(doc.root.as_ref().unwrap()), owners);
    let ready = request.ready_for(&doc).unwrap().unwrap();
    assert_eq!(ready.windows_for(&doc).unwrap().len(), 2);
    assert!(
        ready
            .windows_for(&doc)
            .unwrap()
            .all(|window| window.table_end_for(&doc).unwrap().is_some())
    );
    let mut next = 0;
    for part in ready.coverage_for(&doc).unwrap() {
        assert_eq!(part.utf16_range().start, next);
        next = part.utf16_range().end;
    }
    assert_eq!(next, doc.utf16_len());
    let mut original = MarkdownViewportPresentation::default();
    original.set_active_selection(0..0);
    original
        .rebuild(doc.generation(), viewport, source)
        .unwrap();
    for window in ready.windows_for(&doc).unwrap() {
        let projected = window.presentation_for(&doc).unwrap();
        for range in window.coverage_for(&doc).unwrap() {
            for at in range.clone() {
                let style = |runs: &[MarkdownPresentationRun]| {
                    runs.iter()
                        .find(|run| run.utf16_range.contains(&at))
                        .map_or(MarkdownPresentationStyles::default(), |run| run.styles)
                };
                assert_eq!(style(&projected.runs), style(original.runs()), "at={at}");
            }
        }
    }
    assert!(ready.gaps_for(&doc).unwrap().any(|gap| {
        let range = gap.utf16_range_for(&doc).unwrap();
        range.contains(&units(source, source.find("中间").unwrap()))
    }));
    assert!(request.jobs.is_empty() && request.queries.is_none());
}

#[test]
fn forward_resume_keeps_same_global_math_and_real_absolute_ordinals() {
    let source = "| H |\n| - |\n| ${x |\n| y}$ |\n| tail |\n\n普通段\n\n| N |\n| - |\n| z |\n";
    let (_directory, doc) = document(source);
    let first = source.find("| ${x").unwrap();
    let second = source.find("| y}").unwrap();
    let third = source.find("| tail").unwrap();
    let mut request = doc.table_viewport_request(first..second, 7, 0..0).unwrap();
    finish(&mut request, &doc).unwrap();
    let ptr = request.structure.as_ref().unwrap().global.as_ref().unwrap() as *const GlobalFrontier;
    let before = request.total_work().source_bytes;
    let ready = request.take_ready_for(&doc).unwrap().unwrap();
    assert_eq!(
        ready
            .window_at_for(&doc, 0)
            .unwrap()
            .unwrap()
            .presentation_for(&doc)
            .unwrap()
            .rows[0]
            .row_ordinal,
        1
    );
    assert_eq!(request.forward_resume_floor(), second);
    request
        .resume_forward(&doc, second..third, 8, 0..0)
        .unwrap();
    finish(&mut request, &doc).unwrap();
    assert_eq!(
        ptr,
        request.structure.as_ref().unwrap().global.as_ref().unwrap() as *const GlobalFrontier
    );
    assert!(request.total_work().source_bytes > before);
    let ready = request.take_ready_for(&doc).unwrap().unwrap();
    let window = ready.window_at_for(&doc, 0).unwrap().unwrap();
    assert_eq!(
        window.presentation_for(&doc).unwrap().rows[0].row_ordinal,
        2
    );
    assert!(window.table_end_for(&doc).unwrap().is_none());
    let mut original = MarkdownViewportPresentation::default();
    original.set_active_selection(0..0);
    original
        .rebuild(doc.generation(), 0..doc.utf16_len(), source)
        .unwrap();
    let at = second + 2;
    let style = |runs: &[MarkdownPresentationRun]| {
        runs.iter()
            .find(|run| run.utf16_range.contains(&at))
            .map_or(MarkdownPresentationStyles::default(), |run| run.styles)
    };
    assert_eq!(
        style(&window.presentation_for(&doc).unwrap().runs),
        style(original.runs())
    );
    let floor = request.forward_resume_floor();
    request
        .resume_forward(&doc, floor..doc.utf16_len(), 9, 0..0)
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let ready = request.ready_for(&doc).unwrap().unwrap();
    assert_eq!(ready.windows_for(&doc).unwrap().len(), 2);
    assert_eq!(
        ready
            .window_at_for(&doc, 0)
            .unwrap()
            .unwrap()
            .presentation_for(&doc)
            .unwrap()
            .rows[0]
            .row_ordinal,
        3
    );
    assert!(
        ready
            .window_at_for(&doc, 0)
            .unwrap()
            .unwrap()
            .table_end_for(&doc)
            .unwrap()
            .is_some()
    );
    assert_ne!(ready.gaps_for(&doc).unwrap().len(), 0);
}

#[test]
fn gap_watermark_stops_before_unrelated_far_table_without_false_eof_or_origin_restart() {
    let prefix = "普通\n\n";
    let source = format!(
        "{prefix}{}\n\n| H |\n| - |\n| z |\n",
        "a".repeat(1024 * 1024)
    );
    let (_directory, doc) = document(&source);
    let mut request = doc.table_viewport_request(0..3, 1, 0..0).unwrap();
    finish(&mut request, &doc).unwrap();
    let ready = request.take_ready_for(&doc).unwrap().unwrap();
    assert_eq!(ready.windows_for(&doc).unwrap().len(), 0);
    assert_eq!(ready.gaps_for(&doc).unwrap().len(), 1);
    assert!(request.total_work().source_bytes < 10000);
    assert!(request.structure.as_ref().unwrap().global.is_some());
    // 已分类到真实物理边界之前的请求不能偷偷from0重建；它是显式接口拒绝。
    assert_eq!(
        request.resume_forward(&doc, 0..3, 2, 0..0),
        Err(MarkdownTableRequestError::InvalidRequest)
    );
    let work = request.total_work();
    let floor = request.forward_resume_floor();
    request.extend_from_ready(&doc, &ready, 0..floor).unwrap();
    finish(&mut request, &doc).unwrap();
    let shared = request.take_ready_for(&doc).unwrap().unwrap();
    assert!(Rc::ptr_eq(&ready.gaps[0], &shared.gaps[0]));
    assert_eq!(request.total_work().source_bytes, work.source_bytes);
}

#[test]
fn total_rows_across_two_tables_rejects_without_partial_ready_and_stays_failed() {
    let mut source = String::new();
    for _ in 0..2 {
        source.push_str("| H |\n| - |\n");
        for _ in 0..128 {
            source.push_str("| v |\n");
        }
        source.push('\n');
    }
    let (_directory, doc) = document(&source);
    let mut request = doc
        .table_viewport_request(0..doc.utf16_len(), 1, 0..0)
        .unwrap();
    let error = finish(&mut request, &doc).unwrap_err();
    assert!(
        matches!(error, MarkdownTableRequestError::Window(Error::RowLimit)),
        "实际错误：{error:?}"
    );
    assert!(request.ready.is_none() && request.jobs.is_empty() && request.structure.is_none());
    assert_eq!(request.advance(&doc, false, 65536), Err(error));
    assert_eq!(doc.generation(), 0);
}

#[test]
fn batch_queries_are_sealed_before_single_scan_and_limit_is_shared() {
    let mut source = String::from("[early]: /right\n\n");
    for table in 0..2 {
        source.push_str("| H |\n| - |\n| [early] ");
        for query in 0..600 {
            source.push_str(&format!("[t{table}_{query}] "));
        }
        source.push_str("|\n\n");
    }
    let (_directory, doc) = document(&source);
    let mut request = doc
        .table_viewport_request(0..doc.utf16_len(), 1, 0..0)
        .unwrap();
    let error = finish(&mut request, &doc).unwrap_err();
    assert_eq!(
        error,
        MarkdownTableRequestError::Window(Error::ReferenceLimit)
    );
    assert!(request.queries.is_none() && request.jobs.is_empty() && request.ready.is_none());
    assert_eq!(doc.generation(), 0);
}

#[test]
fn cancel_root_swap_and_edit_undo_do_not_reuse_batch_or_gap_authority() {
    let source = "普通\n\n| H |\n| - |\n| v |\n";
    let (_directory, mut doc) = document(source);
    let mut request = doc
        .table_viewport_request(0..doc.utf16_len(), 1, 0..0)
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let ready = request.take_ready_for(&doc).unwrap().unwrap();
    let (_other_directory, other) = document(source);
    assert!(ready.windows_for(&other).is_err());
    assert!(ready.gaps_for(&other).is_err());
    doc.replace_utf16_range(0..1, "新", MarkdownTextSelection::caret(1))
        .unwrap();
    doc.undo().unwrap();
    assert!(ready.coverage_for(&doc).is_err());
    let mut cancelled = doc
        .table_viewport_request(0..doc.utf16_len(), 1, 0..0)
        .unwrap();
    assert_eq!(
        cancelled.advance(&doc, true, 4096),
        Err(MarkdownTableRequestError::Cancelled)
    );
    assert_eq!(
        cancelled.advance(&doc, false, 4096),
        Err(MarkdownTableRequestError::Cancelled)
    );
    assert!(cancelled.structure.is_none() && cancelled.jobs.is_empty());
}

#[test]
fn overlap_shares_physical_tail_then_continues_same_math_global_gap_and_new_reference_query() {
    let source = "[late]: /first\r\n\r\n| H |\r\n| - |\r\n| ${中 |\r\n| 尾}$ [late] |\r\n| end |\r\n\r\n普通 **段**\r\n\r\n| N |\r\n| - |\r\n| z |\r\n\r\n[other]: /later\r\n";
    let (_directory, doc) = document(source);
    let owners = Arc::strong_count(doc.root.as_ref().unwrap());
    let first = units(source, source.find("| ${中").unwrap());
    let second = units(source, source.find("| 尾}").unwrap());
    let mut request = doc
        .table_viewport_request(first..first + 3, 71, 0..0)
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let old = request.take_ready_for(&doc).unwrap().unwrap();
    let global =
        request.structure.as_ref().unwrap().global.as_ref().unwrap() as *const GlobalFrontier;
    let old_work = request.total_work();
    assert_eq!(request.forward_resume_floor(), second);
    assert!(
        old.window_at_for(&doc, 0)
            .unwrap()
            .unwrap()
            .table_end_for(&doc)
            .unwrap()
            .is_none()
    );
    // 原 requested 在物理行中间截止，完整已验证行尾仍可共享，不能为它重读 source。
    request
        .extend_from_ready(&doc, &old, first..second)
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let tail = request.take_ready_for(&doc).unwrap().unwrap();
    assert_eq!(request.total_work().source_bytes, old_work.source_bytes);
    assert!(Rc::ptr_eq(&old.windows[0], &tail.windows[0]));
    assert_eq!(
        tail.coverage_for(&doc)
            .unwrap()
            .last()
            .unwrap()
            .utf16_range()
            .end,
        second
    );
    request
        .extend_from_ready(&doc, &tail, first..doc.utf16_len())
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let combined = request.take_ready_for(&doc).unwrap().unwrap();
    assert_eq!(
        global,
        request.structure.as_ref().unwrap().global.as_ref().unwrap() as *const GlobalFrontier
    );
    assert!(request.total_work().source_bytes > old_work.source_bytes);
    assert_eq!(Arc::strong_count(doc.root.as_ref().unwrap()), owners);
    assert!(Rc::ptr_eq(&old.windows[0], &combined.windows[0]));
    assert_eq!(combined.windows_for(&doc).unwrap().len(), 3);
    let middle = combined.window_at_for(&doc, 1).unwrap().unwrap();
    assert_eq!(
        middle
            .presentation_for(&doc)
            .unwrap()
            .rows
            .iter()
            .map(|row| row.row_ordinal)
            .collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert!(middle.table_end_for(&doc).unwrap().is_some());
    // 后来的真实 End 只属于新票，不能就地升级原 immutable 窗口的未知末尾。
    assert!(
        old.window_at_for(&doc, 0)
            .unwrap()
            .unwrap()
            .table_end_for(&doc)
            .unwrap()
            .is_none()
    );
    let mut next = first;
    for part in combined.coverage_for(&doc).unwrap() {
        assert_eq!(part.utf16_range().start, next);
        next = part.utf16_range().end;
    }
    assert_eq!(next, doc.utf16_len());
    assert!(combined.gaps_for(&doc).unwrap().any(|gap| {
        gap.utf16
            .contains(&units(source, source.find("普通").unwrap()))
    }));
    // 新后缀才出现 [late]，其定义位于原 viewport 前。只能重新做完整 first-wins 查询，不能从 floor 查。
    let mut original = MarkdownViewportPresentation::default();
    original.set_active_selection(0..0);
    original
        .rebuild(doc.generation(), 0..doc.utf16_len(), source)
        .unwrap();
    let style = |runs: &[MarkdownPresentationRun], at| {
        runs.iter()
            .find(|run| run.utf16_range.contains(&at))
            .map_or(MarkdownPresentationStyles::default(), |run| run.styles)
    };
    for window in combined.windows_for(&doc).unwrap() {
        for range in window.coverage_for(&doc).unwrap() {
            for at in range.clone() {
                assert_eq!(
                    style(&window.presentation_for(&doc).unwrap().runs, at),
                    style(original.runs(), at),
                    "at={at}"
                );
            }
        }
    }
    let at = units(source, source.find("[late] |").unwrap()) + 1;
    assert!(
        style(&middle.presentation_for(&doc).unwrap().runs, at)
            .contains(MarkdownPresentationStyles::LINK)
    );
    assert!(request.queries.is_none() && request.jobs.is_empty());
}

#[test]
fn overlap_rejects_foreign_publication_old_publication_and_unknown_left_hole_before_mutation() {
    let source = "普通\r\n\r\n| H |\r\n| - |\r\n| 一 |\r\n| 二 |\r\n| 三 |\r\n";
    let (_directory, mut doc) = document(source);
    let second = units(source, source.find("| 二").unwrap());
    let third = units(source, source.find("| 三").unwrap());
    let mut request = doc
        .table_viewport_request(second..second + 3, 18, second..second)
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let old = request.take_ready_for(&doc).unwrap().unwrap();
    let before = request.total_work();
    let mut other = doc
        .table_viewport_request(second..second + 3, 18, second..second)
        .unwrap();
    finish(&mut other, &doc).unwrap();
    let foreign = other.take_ready_for(&doc).unwrap().unwrap();
    assert_eq!(
        request.extend_from_ready(&doc, &foreign, second..third),
        Err(MarkdownTableRequestError::PublicationSuperseded)
    );
    assert_eq!(
        request.extend_from_ready(&doc, &old, 0..third),
        Err(MarkdownTableRequestError::CoverageRequiresCheckpoint)
    );
    assert_eq!(request.total_work(), before);
    assert!(matches!(request.phase, BatchPhase::Consumed));
    assert_eq!(
        old.window_at_for(&doc, 0)
            .unwrap()
            .unwrap()
            .presentation_for(&doc)
            .unwrap()
            .rows[0]
            .row_ordinal,
        2
    );
    // 内部洞不能由 H context 到 body 的包络冒充；测试直接复用原真实 coverage 目录作证。
    assert!(!old.proves(&(0..third)));
    request
        .extend_from_ready(&doc, &old, second..third)
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let latest = request.take_ready_for(&doc).unwrap().unwrap();
    let after = request.total_work();
    assert_eq!(
        request.extend_from_ready(&doc, &old, second..doc.utf16_len()),
        Err(MarkdownTableRequestError::PublicationSuperseded)
    );
    assert_eq!(request.total_work(), after);
    assert!(latest.coverage_for(&doc).is_ok() && old.coverage_for(&doc).is_ok());
    let (_other_directory, same_generation_other_root) = document(source);
    assert_eq!(
        request.extend_from_ready(&same_generation_other_root, &latest, second..third),
        Err(MarkdownTableRequestError::SourceSuperseded)
    );
    doc.replace_utf16_range(0..1, "新", MarkdownTextSelection::caret(1))
        .unwrap();
    doc.undo().unwrap();
    assert_eq!(
        request.extend_from_ready(&doc, &latest, second..third),
        Err(MarkdownTableRequestError::SourceSuperseded)
    );
}

#[test]
fn overlap_full_old_row_budget_cannot_be_refunded_by_clipping_or_new_suffix_quota() {
    let mut source = String::from("| H |\n| - |\n");
    for _ in 0..255 {
        source.push_str("| v |\n");
    }
    let (_directory, doc) = document(&source);
    let end = "| H |\n| - |\n".len() + 254 * "| v |\n".len();
    let mut request = doc.table_viewport_request(0..end, 11, 0..0).unwrap();
    finish(&mut request, &doc).unwrap();
    let old = request.take_ready_for(&doc).unwrap().unwrap();
    assert_eq!(old.budget().row_slots, TABLE_WINDOW_MAX_ROWS);
    assert_eq!(request.forward_resume_floor(), end);
    // 即使新请求只与旧最后一行相交，旧 Native Ready 仍持有所有行，额度不能只按可见裁剪退款。
    request
        .extend_from_ready(&doc, &old, end - 1..doc.utf16_len())
        .unwrap();
    let error = finish(&mut request, &doc).unwrap_err();
    assert_eq!(error, MarkdownTableRequestError::Window(Error::RowLimit));
    assert_eq!(request.advance(&doc, false, 4096), Err(error));
    assert!(
        request.structure.is_none() && request.windows.is_empty() && request.job_costs.is_empty()
    );
    assert!(old.coverage_for(&doc).is_ok());
    assert_eq!(old.budget().row_slots, TABLE_WINDOW_MAX_ROWS);
}

#[test]
fn overlap_precharges_real_old_cells_before_individually_valid_suffix() {
    let row = format!("|{}\n", " v |".repeat(256));
    let header = format!("|{}\n", " H |".repeat(256));
    let delimiter = format!("|{}\n", " - |".repeat(256));
    let source = format!("{header}{delimiter}{}", row.repeat(4));
    let (_directory, doc) = document(&source);
    let split = header.len() + delimiter.len() + 2 * row.len();
    let mut request = doc.table_viewport_request(0..split, 19, 0..0).unwrap();
    finish(&mut request, &doc).unwrap();
    let old = request.take_ready_for(&doc).unwrap().unwrap();
    let mut independent = doc
        .table_viewport_request(split..doc.utf16_len(), 19, 0..0)
        .unwrap();
    finish(&mut independent, &doc).unwrap();
    let suffix = independent.take_ready_for(&doc).unwrap().unwrap();
    assert_eq!(old.budget().cells, 768);
    assert_eq!(suffix.budget().cells, 768);
    assert!(old.budget().cells + suffix.budget().cells > TABLE_WINDOW_MAX_CELLS);
    assert!(old.budget().row_slots + suffix.budget().row_slots < TABLE_WINDOW_MAX_ROWS);
    drop(suffix);
    drop(independent);
    request
        .extend_from_ready(&doc, &old, 0..doc.utf16_len())
        .unwrap();
    let error = finish(&mut request, &doc).unwrap_err();
    // 原结构层把 cells 剩余额度转换成 body_limit；零可接纳 body 的既有 typed 出口是 RowLimit。
    // 此处真正耗尽的是 cells：行槽仍远低于 256，不能为测试改错误分类或重置 old 额度。
    assert_eq!(error, MarkdownTableRequestError::Window(Error::RowLimit));
    assert_eq!(request.advance(&doc, false, 4096), Err(error));
    assert!(
        request.ready.is_none() && request.windows.is_empty() && old.coverage_for(&doc).is_ok()
    );
}

#[test]
fn overlap_precharges_real_old_runs_before_individually_valid_semantic_suffix() {
    // 真实配对的 underline 交替产生隐藏、正文和普通 runs，两侧输入都须独立完成。
    // 负例检查最终同活载荷合计，不能用伪造 record 或调小测试专用上限替代原预算。
    let row = format!("| {} |\n", "<u>x</u> ".repeat(1100));
    let source = format!("| H |\n| - |\n{row}{row}");
    let (_directory, doc) = document(&source);
    let split = "| H |\n| - |\n".len() + row.len();
    let mut request = doc.table_viewport_request(0..split, 21, 0..0).unwrap();
    finish(&mut request, &doc).unwrap();
    let old = request.take_ready_for(&doc).unwrap().unwrap();
    let mut independent = doc
        .table_viewport_request(split..doc.utf16_len(), 21, 0..0)
        .unwrap();
    finish(&mut independent, &doc).unwrap();
    let suffix = independent.take_ready_for(&doc).unwrap().unwrap();
    assert!(old.budget().runs < MAX_RUNS && suffix.budget().runs < MAX_RUNS);
    assert!(
        old.budget().runs + suffix.budget().runs > MAX_RUNS,
        "old_runs={}, suffix_runs={}, old_metadata={}, suffix_metadata={}",
        old.budget().runs,
        suffix.budget().runs,
        old.retained.metadata_bytes,
        suffix.retained.metadata_bytes
    );
    assert!(old.retained.metadata_bytes + suffix.retained.metadata_bytes < MAX_METADATA_BYTES);
    assert!(old.budget().source_utf16 + suffix.budget().source_utf16 < TABLE_WINDOW_MAX_UTF16);
    assert!(old.budget().row_slots + suffix.budget().row_slots < TABLE_WINDOW_MAX_ROWS);
    drop(suffix);
    drop(independent);
    request
        .extend_from_ready(&doc, &old, 0..doc.utf16_len())
        .unwrap();
    let error = finish(&mut request, &doc).unwrap_err();
    assert_eq!(
        error,
        MarkdownTableRequestError::Window(Error::SemanticLimit)
    );
    // 实际输出已经用尽联合 run 余额，证明本反例没有被更早的 metadata 拒绝冒充。
    assert_eq!(request.quota.remaining_runs(), 0);
    assert_eq!(request.advance(&doc, false, 65536), Err(error));
    assert!(old.coverage_for(&doc).is_ok());
    assert!(request.structure.is_none() && request.windows.is_empty() && request.jobs.is_empty());
}

#[test]
fn overlap_prunes_departed_publication_records_then_cancel_releases_pending_handles() {
    let source = "| H |\n| - |\n| first |\n| next |\n| third |\n| fourth |\n";
    let (_directory, doc) = document(source);
    let next = source.find("| next").unwrap();
    let third = source.find("| third").unwrap();
    let fourth = source.find("| fourth").unwrap();
    let mut request = doc.table_viewport_request(0..next, 3, 0..0).unwrap();
    finish(&mut request, &doc).unwrap();
    let old = request.take_ready_for(&doc).unwrap().unwrap();
    let departed = Rc::downgrade(&old.windows[0]);
    assert_eq!(Rc::strong_count(&old.windows[0]), 1);
    request.extend_from_ready(&doc, &old, next..third).unwrap();
    assert_eq!(Rc::strong_count(&old.windows[0]), 2);
    finish(&mut request, &doc).unwrap();
    let second = request.take_ready_for(&doc).unwrap().unwrap();
    assert_eq!(second.windows_for(&doc).unwrap().len(), 1);
    assert_eq!(
        second
            .window_at_for(&doc, 0)
            .unwrap()
            .unwrap()
            .presentation_for(&doc)
            .unwrap()
            .rows[0]
            .row_ordinal,
        2
    );
    assert_eq!(second.retained.row_slots, 3);
    assert_eq!(second.budget().row_slots, 6); // 本次 Pending 仍计旧新两窗，发布后才释放离开范围的记录。
    assert_eq!(Rc::strong_count(&old.windows[0]), 1);
    drop(old);
    assert!(departed.upgrade().is_none()); // 新 publication 没有通过父批次链保旧记录。
    request
        .extend_from_ready(&doc, &second, third..fourth)
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let old = request.take_ready_for(&doc).unwrap().unwrap();
    assert_eq!(old.retained.row_slots, 3);
    assert_eq!(old.budget().row_slots, 6);
    assert_eq!(
        old.window_at_for(&doc, 0)
            .unwrap()
            .unwrap()
            .presentation_for(&doc)
            .unwrap()
            .rows[0]
            .row_ordinal,
        3
    );
    drop(second);
    request
        .extend_from_ready(&doc, &old, third..doc.utf16_len())
        .unwrap();
    assert_eq!(Rc::strong_count(&old.windows[0]), 2);
    assert_eq!(
        request.advance(&doc, true, 4096),
        Err(MarkdownTableRequestError::Cancelled)
    );
    assert_eq!(
        request.advance(&doc, false, 65536),
        Err(MarkdownTableRequestError::Cancelled)
    );
    assert_eq!(Rc::strong_count(&old.windows[0]), 1);
    assert!(request.structure.is_none() && request.queries.is_none() && request.gaps.is_empty());
    assert!(
        old.window_at_for(&doc, 0)
            .unwrap()
            .unwrap()
            .presentation_for(&doc)
            .is_ok()
    );
}

fn required_window_oracle(
    doc: &MarkdownDocumentBuffer,
    header: usize,
    row: usize,
) -> VerifiedMarkdownTableWindow {
    // 原独立producer只作为测试oracle；生产required路径不能用它拼接publication。
    let mut request = doc
        .table_window_request(
            MarkdownTableWindowTarget::RequiredRow {
                header_byte_start: header,
                row_ordinal: row,
            },
            0..0,
            29,
            0..0,
        )
        .unwrap();
    for _ in 0..doc.byte_len().saturating_mul(64).saturating_add(10000) {
        match request.advance(doc, false, 4096).unwrap() {
            MarkdownTableWindowStep::Ready { .. } => {
                return request.take_ready_for(doc).unwrap().unwrap();
            }
            MarkdownTableWindowStep::Progress { .. } => {}
            other => panic!("真实目标缺少oracle窗口：{other:?}"),
        }
    }
    panic!("有限原producer未完成");
}

#[test]
fn required_previous_replays_same_table_math_and_offsets_without_restarting_main_global() {
    let source = "[a]: /first\r\n\r\n> - | H | C |\r\n>   | - | - |\r\n>   | ${中 | 甲 |\r\n>   | 😀 | 乙 |\r\n>   | y}$ [a] | **三** |\r\n>   | 四 | 尾 |\r\n\r\n[a]: /wrong\r\n";
    let (_directory, doc) = document(source);
    let fourth = units(source, source.find(">   | 四").unwrap());
    let header = source.find("> - | H").unwrap();
    let viewport = fourth..fourth + 4;
    let mut request = doc
        .table_viewport_request(viewport.clone(), 29, 0..0)
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let old = request.take_ready_for(&doc).unwrap().unwrap();
    let global =
        request.structure.as_ref().unwrap().global.as_ref().unwrap() as *const GlobalFrontier;
    let next_byte = request
        .structure
        .as_ref()
        .unwrap()
        .global
        .as_ref()
        .unwrap()
        .next_byte();
    let owners = Arc::strong_count(doc.root.as_ref().unwrap());
    let work = request.total_work();
    let query = old.required_row_query_for(&doc, 0, 4, 3).unwrap();
    request
        .extend_from_ready_with_required_row(&doc, &old, viewport, query)
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let ready = request.take_ready_for(&doc).unwrap().unwrap();
    let index = match ready.required_row_result_for(&doc).unwrap().unwrap() {
        MarkdownTableViewportRequiredRowResult::Row {
            window_index,
            row_ordinal,
        } => {
            assert_eq!(row_ordinal, 3);
            window_index
        }
        _ => panic!("Previous不能被表尾结果冒充"),
    };
    let actual = ready.window_at_for(&doc, index).unwrap().unwrap();
    let oracle = required_window_oracle(&doc, header, 3);
    assert_eq!(
        actual.presentation_for(&doc).unwrap(),
        oracle.presentation_for(&doc).unwrap()
    );
    assert_eq!(
        actual.coverage_for(&doc).unwrap(),
        oracle.coverage_for(&doc).unwrap()
    );
    assert_eq!(
        actual.replay.as_ref().unwrap().chunk.math,
        oracle.replay.as_ref().unwrap().chunk.math
    );
    assert_eq!(
        global,
        request.structure.as_ref().unwrap().global.as_ref().unwrap() as *const GlobalFrontier
    );
    assert_eq!(
        next_byte,
        request
            .structure
            .as_ref()
            .unwrap()
            .global
            .as_ref()
            .unwrap()
            .next_byte()
    );
    assert!(request.total_work().source_bytes > work.source_bytes);
    assert_eq!(Arc::strong_count(doc.root.as_ref().unwrap()), owners);
    assert!(
        old.window_at_for(&doc, 0)
            .unwrap()
            .unwrap()
            .presentation_for(&doc)
            .unwrap()
            .row(4)
            .is_some()
    );
    assert_eq!(
        ready
            .coverage_for(&doc)
            .unwrap()
            .iter()
            .map(|part| part.utf16_range().len())
            .sum::<usize>(),
        4
    );
}

#[test]
fn required_far_previous_and_header_use_retained_real_base_not_a_row_history() {
    let mut source = "| H |\n| - |\n".to_owned();
    for row in 1..=300 {
        source.push_str(&format!("| 行{row}😀 |\n"));
    }
    let (_directory, doc) = document(&source);
    let at = units(&source, source.find("| 行290😀").unwrap());
    let viewport = at..at + 4;
    let mut request = doc
        .table_viewport_request(viewport.clone(), 29, 0..0)
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let old = request.take_ready_for(&doc).unwrap().unwrap();
    let query = old.required_row_query_for(&doc, 0, 290, 1).unwrap();
    request
        .extend_from_ready_with_required_row(&doc, &old, viewport.clone(), query)
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let first = request.take_ready_for(&doc).unwrap().unwrap();
    let index = match first.required_row_result_for(&doc).unwrap().unwrap() {
        MarkdownTableViewportRequiredRowResult::Row {
            window_index,
            row_ordinal: 1,
        } => window_index,
        _ => panic!("远距Previous缺少真实首行"),
    };
    assert_eq!(
        first
            .window_at_for(&doc, index)
            .unwrap()
            .unwrap()
            .presentation_for(&doc)
            .unwrap(),
        required_window_oracle(&doc, 0, 1)
            .presentation_for(&doc)
            .unwrap()
    );
    let query = first.required_row_query_for(&doc, index, 1, 0).unwrap();
    request
        .extend_from_ready_with_required_row(&doc, &first, viewport, query)
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let header = request.take_ready_for(&doc).unwrap().unwrap();
    let index = match header.required_row_result_for(&doc).unwrap().unwrap() {
        MarkdownTableViewportRequiredRowResult::Row {
            window_index,
            row_ordinal: 0,
        } => window_index,
        _ => panic!("真实H不能以context-only行或End代替"),
    };
    let h = header.window_at_for(&doc, index).unwrap().unwrap();
    assert!(h.presentation_for(&doc).unwrap().row(0).unwrap().is_header);
    assert_eq!(h.coverage_for(&doc).unwrap()[0].start, 0);
    assert!(std::mem::size_of::<ReplayAnchors>() <= 4096);
    assert!(header.windows_for(&doc).unwrap().len() <= 3);
    // Header目标发布的是实际row0；随后Next仍核同publication，不把header_context当body。
    let query = header.required_row_query_for(&doc, index, 0, 1).unwrap();
    request
        .extend_from_ready_with_required_row(&doc, &header, at..at + 4, query)
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let next = request.take_ready_for(&doc).unwrap().unwrap();
    assert!(matches!(
        next.required_row_result_for(&doc).unwrap(),
        Some(MarkdownTableViewportRequiredRowResult::Row { row_ordinal: 1, .. })
    ));
}

#[test]
fn required_real_end_same_publication_appends_draft_in_one_undo() {
    let source = "> - | H | C |\r\n>   | - | - |\r\n>   | 中 | 原 |";
    let (_directory, mut doc) = document(source);
    let at = units(source, source.find(">   | 中").unwrap());
    let viewport = at..at + 4;
    let mut request = doc
        .table_viewport_request(viewport.clone(), 29, 0..0)
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let old = request.take_ready_for(&doc).unwrap().unwrap();
    assert!(
        old.window_at_for(&doc, 0)
            .unwrap()
            .unwrap()
            .table_end_for(&doc)
            .unwrap()
            .is_none()
    );
    let query = old.required_row_query_for(&doc, 0, 1, 2).unwrap();
    request
        .extend_from_ready_with_required_row(&doc, &old, viewport, query)
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let ready = request.take_ready_for(&doc).unwrap().unwrap();
    let index = match ready.required_row_result_for(&doc).unwrap().unwrap() {
        MarkdownTableViewportRequiredRowResult::TableEnd {
            end,
            terminal_window_index: index,
        } => {
            assert!(end.ended_at_source_eof());
            assert!(!end.ended_with_eol());
            assert_eq!(end.last_body_row_ordinal(), Some(1));
            index
        }
        _ => panic!("不存在行必须返回真实End，不是空Ready"),
    };
    assert!(Rc::ptr_eq(&old.windows[0], &ready.windows[index]));
    let plan = doc
        .plan_verified_table_viewport_row_append(&ready, index, 1, 1, "草稿😀")
        .unwrap();
    let (range, replacement) = plan.replacement_for(&doc).unwrap();
    let range = range.clone();
    let replacement = replacement.to_owned();
    doc.replace_utf16_range(range, &replacement, MarkdownTextSelection::caret(0))
        .unwrap();
    assert_eq!(
        doc.copy_table_source_range(0..doc.byte_len(), 65536)
            .unwrap(),
        "> - | H | C |\r\n>   | - | - |\r\n>   | 中 | 草稿😀 |\r\n>   | | |"
    );
    assert!(plan.replacement_for(&doc).is_err());
    doc.undo().unwrap();
    assert_eq!(
        doc.copy_table_source_range(0..doc.byte_len(), 65536)
            .unwrap(),
        source
    );
    assert!(ready.required_row_result_for(&doc).is_err());
}

#[test]
fn required_union_cells_rejects_without_revoking_previous_and_query_cannot_cross_publication() {
    let mut source = format!("|{}\n|{}\n", " H |".repeat(64), " - |".repeat(64));
    let first = source.len();
    for _ in 0..14 {
        source.push_str(&format!("|{}\n", " x |".repeat(64)));
    }
    let after_fourteen = source.len();
    source.push_str(&format!("|{}\n", " y |".repeat(64)));
    let (_directory, doc) = document(&source);
    let viewport = first..after_fourteen;
    let mut request = doc
        .table_viewport_request(viewport.clone(), 29, 0..0)
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let old = request.take_ready_for(&doc).unwrap().unwrap();
    assert_eq!(old.budget().cells, 960);
    let mut other = doc
        .table_viewport_request(viewport.clone(), 29, 0..0)
        .unwrap();
    finish(&mut other, &doc).unwrap();
    let foreign = other.take_ready_for(&doc).unwrap().unwrap();
    let foreign_query = foreign.required_row_query_for(&doc, 0, 14, 15).unwrap();
    assert_eq!(
        request.extend_from_ready_with_required_row(&doc, &old, viewport.clone(), foreign_query),
        Err(MarkdownTableRequestError::PublicationSuperseded)
    );
    let query = old.required_row_query_for(&doc, 0, 14, 15).unwrap();
    request
        .extend_from_ready_with_required_row(&doc, &old, viewport, query)
        .unwrap();
    assert_eq!(
        finish(&mut request, &doc),
        Err(MarkdownTableRequestError::Window(Error::CellLimit))
    );
    assert!(old.windows_for(&doc).unwrap().next().is_some());
    assert_eq!(
        required_window_oracle(&doc, 0, 15)
            .presentation_for(&doc)
            .unwrap()
            .column_count,
        64
    );
    assert_eq!(
        request.advance(&doc, false, 4096),
        Err(MarkdownTableRequestError::Window(Error::CellLimit))
    );
}

#[test]
fn true_table_checkpoint_one_byte_frames_match_original_global_and_require_real_eof() {
    use pulldown_cmark::block_frontier::{TableContinuationState, TableStep};
    let source = "> - | H | C |\r\n>   | - | - |\r\n>   | 中😀 | x |\r\n>   | 尾 | y |";
    let (_directory, doc) = document(source);
    let mut global = new_global(&doc).unwrap();
    let mut checkpoint = None;
    let mut expected = Vec::new();
    let mut global_complete = false;
    for _ in 0..source.len() * 4096 + 10000 {
        let frame = doc.table_source_frame(global.next_byte(), 1).unwrap();
        match global
            .advance(
                SourceFrame {
                    source_revision: frame.generation,
                    byte_start: frame.byte_start,
                    bytes: &frame.bytes,
                    real_eof: frame.real_eof,
                },
                GlobalSourceAuthority {
                    current_revision: doc.generation(),
                    cancelled: false,
                },
                4096,
            )
            .unwrap()
        {
            GlobalStep::Fact {
                fact: GlobalFact::Table(_),
                ..
            } => checkpoint = global.table_checkpoint(),
            GlobalStep::Fact {
                fact: GlobalFact::Row(row),
                ..
            } => expected.push(row),
            GlobalStep::Complete { .. } => {
                global_complete = true;
                break;
            }
            _ => {}
        }
    }
    assert!(global_complete);
    let mut table = TableContinuationState::resume(checkpoint.unwrap());
    let mut actual = Vec::new();
    let mut denied_false_eof = false;
    let mut table_complete = false;
    for _ in 0..source.len() * 4096 + 10000 {
        let frame = doc.table_source_frame(table.next_byte(), 1).unwrap();
        let is_empty_end = frame.bytes.is_empty() && frame.real_eof;
        if is_empty_end && !denied_false_eof {
            assert!(matches!(
                table
                    .advance_bounded(
                        SourceFrame {
                            source_revision: frame.generation,
                            byte_start: frame.byte_start,
                            bytes: &frame.bytes,
                            real_eof: false
                        },
                        1,
                        4096
                    )
                    .unwrap(),
                TableStep::NeedInput { .. }
            ));
            assert!(table.verified_end().is_none());
            denied_false_eof = true;
        }
        let step = table
            .advance_bounded(
                SourceFrame {
                    source_revision: frame.generation,
                    byte_start: frame.byte_start,
                    bytes: &frame.bytes,
                    real_eof: frame.real_eof,
                },
                1,
                4096,
            )
            .unwrap();
        assert!(table.last_work().byte_reads <= 1 && table.last_work().transitions <= 4096);
        match step {
            TableStep::Row { fact, .. } => actual.push(fact),
            TableStep::Ended { .. } => {
                table_complete = true;
                break;
            }
            TableStep::Progress { .. } | TableStep::NeedInput { .. } => {
                if table.last_work().byte_reads > 0 {
                    assert!(table.checkpoint().is_none());
                }
            }
        }
    }
    assert!(table_complete);
    assert_eq!(actual, expected);
    assert!(denied_false_eof);
    let end = table.verified_end().unwrap();
    assert!(end.ended_at_source_eof());
    assert_eq!(end.last_body_row_ordinal(), Some(2));
}

#[test]
fn required_partial_replay_cancel_and_stale_source_keep_old_ready_and_no_source_owner() {
    let source = format!("| H |\n| - |\n| {} |\n| final |\n", "中😀".repeat(3000));
    let (_directory, mut doc) = document(&source);
    let at = units(&source, source.find("| final").unwrap());
    let viewport = at..at + 4;
    let mut request = doc
        .table_viewport_request(viewport.clone(), 29, 0..0)
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let old = request.take_ready_for(&doc).unwrap().unwrap();
    let owners = Arc::strong_count(doc.root.as_ref().unwrap());
    let query = old.required_row_query_for(&doc, 0, 2, 1).unwrap();
    request
        .extend_from_ready_with_required_row(&doc, &old, viewport, query)
        .unwrap();
    assert!(matches!(
        request.advance(&doc, false, 4096).unwrap(),
        MarkdownTableViewportStep::Progress { .. }
    ));
    assert!(request.ready_for(&doc).unwrap().is_none());
    assert_eq!(
        request.advance(&doc, true, 4096),
        Err(MarkdownTableRequestError::Cancelled)
    );
    assert_eq!(
        request.advance(&doc, false, 4096),
        Err(MarkdownTableRequestError::Cancelled)
    );
    assert!(request.required.is_none() && request.structure.is_none());
    assert_eq!(Arc::strong_count(doc.root.as_ref().unwrap()), owners);
    assert!(
        old.window_at_for(&doc, 0)
            .unwrap()
            .unwrap()
            .presentation_for(&doc)
            .unwrap()
            .row(2)
            .is_some()
    );
    let (_other_directory, other) = document(&source);
    assert!(old.required_row_query_for(&other, 0, 2, 1).is_err());
    doc.replace_utf16_range(2..3, "X", MarkdownTextSelection::caret(3))
        .unwrap();
    doc.undo().unwrap();
    assert!(old.required_row_query_for(&doc, 0, 2, 1).is_err());
}

#[test]
fn required_far_beyond_end_publishes_actual_terminal_row_and_implicit_columns() {
    let source = "| H | C |\n| - | - |\n| first | here |\n| last |\n";
    let (_directory, doc) = document(source);
    let at = source.find("| first").unwrap();
    let viewport = at..at + 4;
    let mut request = doc
        .table_viewport_request(viewport.clone(), 29, 0..0)
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let old = request.take_ready_for(&doc).unwrap().unwrap();
    let query = old.required_row_query_for(&doc, 0, 1, 999).unwrap();
    request
        .extend_from_ready_with_required_row(&doc, &old, viewport, query)
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let ready = request.take_ready_for(&doc).unwrap().unwrap();
    let index = match ready.required_row_result_for(&doc).unwrap().unwrap() {
        MarkdownTableViewportRequiredRowResult::TableEnd {
            end,
            terminal_window_index,
        } => {
            assert_eq!(end.last_body_row_ordinal(), Some(2));
            terminal_window_index
        }
        _ => panic!("超界只准真实End与实际末行"),
    };
    let terminal = ready
        .window_at_for(&doc, index)
        .unwrap()
        .unwrap()
        .presentation_for(&doc)
        .unwrap();
    assert_eq!(terminal.column_count, 2);
    assert!(matches!(&terminal.cell(2, 1).unwrap().source,
        crate::markdown_viewport_presentation::table_window::MarkdownTableWindowCellSource::ImplicitEmpty { .. }));
    assert!(
        doc.plan_verified_table_viewport_row_append(&ready, index, 1, 1, "draft")
            .is_err()
    );
    assert!(
        doc.plan_verified_table_viewport_row_append(&ready, index, 2, 0, "draft")
            .is_err()
    );
    assert_eq!(
        doc.plan_verified_table_viewport_row_append(&ready, index, 2, 1, "draft")
            .unwrap()
            .next_cell(),
        (3, 0)
    );
}

fn assert_required_source_partition(
    ready: &VerifiedMarkdownTableViewport,
    doc: &MarkdownDocumentBuffer,
) {
    let requested = ready.requested_for(doc).unwrap();
    let mut next = requested.start;
    for coverage in ready.coverage_for(doc).unwrap() {
        // 同一源位置只能发布一次；重复target ticket不能以重叠区间通过连续覆盖认证。
        assert_eq!(coverage.utf16_range().start, next);
        next = coverage.utf16_range().end;
    }
    assert_eq!(next, requested.end);
}

#[test]
fn required_suffix_target_reuses_sweep_ticket_without_duplicate_row_or_coverage() {
    let source = "| H |\n| - |\n| 一 |\n| 二 |\n";
    let (_directory, doc) = document(source);
    let first = units(source, source.find("| 一").unwrap());
    let mut request = doc
        .table_viewport_request(first..first + 4, 29, 0..0)
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let old = request.take_ready_for(&doc).unwrap().unwrap();
    let query = old.required_row_query_for(&doc, 0, 1, 2).unwrap();
    request
        .extend_from_ready_with_required_row(&doc, &old, first..doc.utf16_len(), query)
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let ready = request.take_ready_for(&doc).unwrap().unwrap();
    let index = match ready.required_row_result_for(&doc).unwrap().unwrap() {
        MarkdownTableViewportRequiredRowResult::Row {
            window_index,
            row_ordinal: 2,
        } => window_index,
        _ => panic!("本轮Sweep的真实row2必须进入同一Ready"),
    };
    assert!(
        ready
            .window_at_for(&doc, index)
            .unwrap()
            .unwrap()
            .presentation_for(&doc)
            .unwrap()
            .row(2)
            .is_some()
    );
    assert_eq!(
        ready
            .windows_for(&doc)
            .unwrap()
            .filter(|window| window.presentation_for(&doc).unwrap().row(2).is_some())
            .count(),
        1
    );
    assert_eq!(ready.budget().cells, 4);
    assert_eq!(ready.budget().row_slots, 6);
    assert_required_source_partition(&ready, &doc);
    assert!(
        old.window_at_for(&doc, 0)
            .unwrap()
            .unwrap()
            .presentation_for(&doc)
            .unwrap()
            .row(1)
            .is_some()
    );
}

#[test]
fn required_suffix_terminal_reuses_sweep_ticket_and_retains_true_end_association() {
    let source = "| H |\n| - |\n| 一 |\n| 二 |\n";
    let (_directory, doc) = document(source);
    let first = units(source, source.find("| 一").unwrap());
    let mut request = doc
        .table_viewport_request(first..first + 4, 29, 0..0)
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let old = request.take_ready_for(&doc).unwrap().unwrap();
    let query = old.required_row_query_for(&doc, 0, 1, 999).unwrap();
    request
        .extend_from_ready_with_required_row(&doc, &old, first..doc.utf16_len(), query)
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let ready = request.take_ready_for(&doc).unwrap().unwrap();
    let index = match ready.required_row_result_for(&doc).unwrap().unwrap() {
        MarkdownTableViewportRequiredRowResult::TableEnd {
            end,
            terminal_window_index,
        } => {
            assert_eq!(end.last_body_row_ordinal(), Some(2));
            assert!(end.ended_at_source_eof());
            terminal_window_index
        }
        _ => panic!("真实末行不得由重复ticket或超界目标代替"),
    };
    assert_eq!(
        ready
            .windows_for(&doc)
            .unwrap()
            .filter(|window| window.presentation_for(&doc).unwrap().row(2).is_some())
            .count(),
        1
    );
    assert_eq!(ready.budget().cells, 4);
    assert_eq!(ready.budget().row_slots, 6);
    assert_required_source_partition(&ready, &doc);
    assert_eq!(
        doc.plan_verified_table_viewport_row_append(&ready, index, 2, 0, "草稿")
            .unwrap()
            .next_cell(),
        (3, 0)
    );
}

#[test]
fn required_repeated_replay_exhausts_original_total_fuel_and_preserves_previous_and_draft_plan() {
    let source = "| H |\n| - |\n| one |\n| two |\n| three |\n| four |\n";
    let (_directory, doc) = document(source);
    let fourth = source.find("| four").unwrap();
    let viewport = fourth..fourth + 4;
    let selection = doc.selection();
    let mut request = doc
        .table_viewport_request(viewport.clone(), 29, 0..0)
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let mut previous = request.take_ready_for(&doc).unwrap().unwrap();
    let plan = doc
        .plan_verified_table_cell_edit(
            previous.window_at_for(&doc, 0).unwrap().unwrap(),
            4,
            0,
            "未提交草稿😀",
        )
        .unwrap();
    let draft = plan.replacement_for(&doc).unwrap().1.to_owned();
    let max_iterations = request.structure.as_ref().unwrap().max_transitions / 64 + 1;
    let mut origin_index = 0;
    let mut origin_row = 4;
    let mut target = 1;
    let mut exhausted = false;
    for _ in 0..max_iterations {
        let query = previous
            .required_row_query_for(&doc, origin_index, origin_row, target)
            .unwrap();
        request
            .extend_from_ready_with_required_row(&doc, &previous, viewport.clone(), query)
            .unwrap();
        match finish(&mut request, &doc) {
            Ok(()) => {
                // 三个目标循环会越过只保当前origin/target的目录，确实重复回放而非只读已有窗。
                let ready = request.take_ready_for(&doc).unwrap().unwrap();
                origin_index = match ready.required_row_result_for(&doc).unwrap().unwrap() {
                    MarkdownTableViewportRequiredRowResult::Row {
                        window_index,
                        row_ordinal,
                    } => {
                        assert_eq!(row_ordinal, target);
                        window_index
                    }
                    _ => panic!("fuel夹具全部目标均是真实成员"),
                };
                previous = ready;
                origin_row = target;
                target = target % 3 + 1;
            }
            Err(error) => {
                assert_eq!(error, MarkdownTableRequestError::WorkLimit);
                exhausted = true;
                break;
            }
        }
    }
    assert!(exhausted, "原共享总fuel必须耗尽，不能为每次RequiredRow重授");
    assert_eq!(
        request.advance(&doc, false, 4096),
        Err(MarkdownTableRequestError::WorkLimit)
    );
    assert!(
        previous
            .window_at_for(&doc, origin_index)
            .unwrap()
            .unwrap()
            .presentation_for(&doc)
            .unwrap()
            .row(origin_row)
            .is_some()
    );
    assert_eq!(plan.replacement_for(&doc).unwrap().1, draft);
    assert_eq!(doc.generation(), 0);
    assert_eq!(doc.selection(), selection);
    assert_eq!(
        doc.copy_table_source_range(0..doc.byte_len(), 65536)
            .unwrap(),
        source
    );
}

#[test]
fn required_header_delimiter_only_end_has_real_row_zero_and_single_append_undo() {
    let source = "| H | C |\r\n| - | - |";
    let (_directory, mut doc) = document(source);
    let mut request = doc.table_viewport_request(0..4, 29, 0..0).unwrap();
    finish(&mut request, &doc).unwrap();
    let old = request.take_ready_for(&doc).unwrap().unwrap();
    let query = old.required_row_query_for(&doc, 0, 0, 1).unwrap();
    request
        .extend_from_ready_with_required_row(&doc, &old, 0..4, query)
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let ready = request.take_ready_for(&doc).unwrap().unwrap();
    let index = match ready.required_row_result_for(&doc).unwrap().unwrap() {
        MarkdownTableViewportRequiredRowResult::TableEnd {
            end,
            terminal_window_index,
        } => {
            assert_eq!(end.last_body_row_ordinal(), None);
            assert!(!end.ended_with_eol() && end.ended_at_source_eof());
            terminal_window_index
        }
        _ => panic!("H/D-only必须保原H与真实EOF，不能伪造body1"),
    };
    let window = ready.window_at_for(&doc, index).unwrap().unwrap();
    assert!(
        window
            .presentation_for(&doc)
            .unwrap()
            .row(0)
            .unwrap()
            .is_header
    );
    assert!(window.presentation_for(&doc).unwrap().row(1).is_none());
    let coverage = window.coverage_for(&doc).unwrap();
    assert_eq!(coverage.len(), 1);
    assert_eq!(coverage[0], 0..doc.utf16_len());
    let plan = doc
        .plan_verified_table_viewport_row_append(&ready, index, 0, 1, "草稿")
        .unwrap();
    let (range, replacement) = plan.replacement_for(&doc).unwrap();
    let range = range.clone();
    let replacement = replacement.to_owned();
    doc.replace_utf16_range(range, &replacement, MarkdownTextSelection::caret(0))
        .unwrap();
    assert_eq!(
        doc.copy_table_source_range(0..doc.byte_len(), 65536)
            .unwrap(),
        "| H | 草稿 |\r\n| - | - |\r\n| | |"
    );
    doc.undo().unwrap();
    assert_eq!(
        doc.copy_table_source_range(0..doc.byte_len(), 65536)
            .unwrap(),
        source
    );
    assert!(plan.replacement_for(&doc).is_err());
}
