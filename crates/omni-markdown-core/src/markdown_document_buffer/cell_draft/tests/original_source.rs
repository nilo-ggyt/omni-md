//! 原文件已有长格的真实激活、失败隔离与保存历史；不手造语义窗口或整格编辑副本。

use super::*;

/// 实际结构请求再流式取格端点；纯测试循环不参与产品事件调度。
pub(super) fn plan(
    doc: &MarkdownDocumentBuffer,
    row: usize,
    column: usize,
) -> MarkdownTableCellEditPlan {
    let mut request = doc.table_cell_edit_request(0, row, column).unwrap();
    loop {
        match request.advance(doc, false, 4096).unwrap() {
            MarkdownTableCellEditStep::Progress { .. } => {}
            MarkdownTableCellEditStep::Ready { .. } => {
                return request.take_plan_for(doc).unwrap().unwrap();
            }
            other => panic!("真实原格没有编辑计划：{other:?}"),
        }
    }
}

#[test]
/// 流式词法与原有限Parser在同一真实源上给出相同替换及缺格前缀；不维护另一份GFM oracle。
fn streamed_cells_match_original_window_plans_for_escapes_padding_and_missing_cells() {
    for prefix in ["", "> > ", "   "] {
        for row in [
            "| a | 中😀 |",
            "|a||",
            "a\\|b",
            "a\\\\|",
            "`a|b`",
            "a",
            "|a|  ",
            "| a\u{b} | b\u{c} |",
        ] {
            let source = format!(
                "{prefix}| A | B | C | D |\r\n{prefix}| - | - | - | - |\r\n{prefix}{row}\r\n"
            );
            let (_directory, doc) = document(&source);
            let verified = window(&doc);
            for column in 0..4 {
                // 原正式短格准备者和新原格请求必须最终消费同一个plan职责。
                let expected = doc
                    .plan_verified_table_cell_edit(&verified, 1, column, "")
                    .unwrap();
                let actual = plan(&doc, 1, column);
                assert_eq!(
                    actual.replacement_for(&doc).unwrap(),
                    expected.replacement_for(&doc).unwrap(),
                    "{source:?}, column={column}"
                );
            }
        }
    }
}

#[test]
/// 原公开替换计划可带正文；激活不能悄悄将它保成补格前缀或发布文本。
fn nonempty_explicit_replacement_plan_is_rejected_before_creating_a_draft() {
    let (_directory, mut doc) = document(SOURCE);
    let verified = window(&doc);
    let replacement = doc
        .plan_verified_table_cell_edit(&verified, 1, 0, "替换正文")
        .unwrap();
    let before = doc.storage_profile();
    assert!(matches!(
        doc.begin_prepared_cell_draft(replacement),
        Err(MarkdownCellDraftError::Table(
            MarkdownTableWindowError::InvalidSource
        ))
    ));
    assert_eq!(doc.storage_profile(), before);
    assert_eq!(canonical(&doc), SOURCE);
    assert!(!doc.has_cell_draft());
    let empty = plan(&doc, 1, 0);
    let token = doc.begin_prepared_cell_draft(empty).unwrap();
    assert!(doc.cancel_cell_draft(&token));
}

#[test]
/// 原2MiB格从文件映射切出；两份真实Reader不阻止准备，归还后保存和Undo/Redo只发布一次。
fn existing_two_mib_cell_edits_and_saves_without_whole_cell_materialization() {
    let mut payload = "x".repeat(2 * 1024 * 1024 - "中😀".len());
    payload.push_str("中😀");
    let source = format!("| H | C |\n| --- | --- |\n| {payload} | other |\n");
    let (directory, original) = document(&source);
    drop(original);
    let workspace = crate::MarkdownWorkspace::new(
        directory.path().to_path_buf(),
        directory.path().join("app-data"),
    );
    let access = workspace.open_session().unwrap();
    let mut doc = access.open_document_buffer("cell-draft.md").unwrap();
    let prepared = plan(&doc, 1, 0);
    let token = doc.begin_prepared_cell_draft(prepared).unwrap();
    let facts = doc.cell_draft_facts(&token).unwrap();
    assert_eq!(facts.byte_len, 2 * 1024 * 1024);
    assert_eq!(doc.storage_profile().allocated_edit_segment_bytes, 0);
    edit(
        &mut doc,
        &token,
        facts.utf16_len..facts.utf16_len,
        "尾",
        MarkdownTextSelection::caret(facts.utf16_len + 1),
        None,
    )
    .unwrap();
    let first = doc.capture_cell_draft(&token).unwrap();
    let second = doc.capture_cell_draft(&token).unwrap();
    let mut tail = [0; 4];
    first
        .copy_utf16_code_units(&first.authority(), facts.utf16_len - 3, &mut tail)
        .unwrap();
    assert_eq!(String::from_utf16(&tail).unwrap(), "中😀尾");
    let prepared = doc
        .prepare_cell_draft_commit(doc.capture_cell_draft_edit(&token).unwrap())
        .unwrap();
    let candidate = prepared.replacement_backing_observer().unwrap();
    assert!(matches!(
        doc.commit_prepared_cell_draft_observed(prepared, NoopObserver),
        Err(MarkdownCellDraftCommitError::Draft(
            MarkdownCellDraftError::ReadersBusy
        ))
    ));
    assert!(candidate.unmap_succeeded());
    drop((first, second)); // 同一原读取值真正归还后重新准备，不能重复消费旧准入票。
    let prepared = doc
        .prepare_cell_draft_commit(doc.capture_cell_draft_edit(&token).unwrap())
        .unwrap();
    let mapping = prepared.replacement_backing_observer().unwrap();
    doc.commit_prepared_cell_draft_observed(prepared, NoopObserver)
        .unwrap()
        .unwrap();
    let expected = source.replacen("中😀 |", "中😀尾 |", 1);
    access.save_document_buffer(&mut doc).unwrap();
    assert_eq!(std::fs::read(doc.path()).unwrap(), expected.as_bytes());
    assert_eq!(doc.storage_profile().undo_entries, 1);
    doc.undo_text_edit().unwrap();
    assert_eq!(canonical(&doc), source);
    assert!(doc.is_dirty());
    doc.redo_text_edit().unwrap();
    assert_eq!(canonical(&doc), expected);
    assert!(!doc.is_dirty());
    drop(doc);
    assert!(mapping.unmap_succeeded());
}

#[test]
/// 文档释放票覆盖真正发布的文件段及其历史；准备候选与raw分支仍归各自事务。
fn document_backing_observers_follow_published_file_through_undo_redo_and_drop() {
    let (_directory, mut doc) = document(SOURCE);
    let original = doc.original_backing_observer().unwrap();
    let token = begin(&mut doc);
    let text = "x".repeat(80_000);
    replace(&mut doc, &token, &text);
    let prepared = doc
        .prepare_cell_draft_commit(doc.capture_cell_draft_edit(&token).unwrap())
        .unwrap();
    let candidate = prepared.replacement_backing_observer().unwrap();
    // 两个尚未发布的实际文件都不能成为canonical文档的释放责任。
    assert!(
        doc.backing_observers()
            .all(|backing| backing.mapped_bytes() == SOURCE.len())
    );
    doc.commit_prepared_cell_draft_observed(prepared, NoopObserver)
        .unwrap()
        .unwrap();
    let expected = SOURCE.replacen("| a |", &format!("| {text} |"), 1);
    assert_eq!(canonical(&doc), expected);
    let published = doc
        .backing_observers()
        .find(|backing| backing.mapped_bytes() == text.len())
        .expect("实际发布的文件段必须进入文档最终释放票");
    assert!(!candidate.unmap_succeeded());
    doc.undo_text_edit().unwrap();
    assert_eq!(canonical(&doc), SOURCE);
    assert!(
        doc.backing_observers()
            .any(|backing| backing.mapped_bytes() == text.len())
    );
    assert!(!published.unmap_succeeded()); // Undo保留真实Redo正文，不能提前报告释放。
    doc.redo_text_edit().unwrap();
    assert_eq!(canonical(&doc), expected);
    let backings: Vec<_> = doc.backing_observers().collect();
    drop(doc);
    // observer与弱token都不延长映射寿命；成功结论来自每个实际munmap结果。
    for backing in backings {
        assert!(backing.unmap_succeeded());
        assert_eq!(backing.mapped_bytes(), 0);
        assert!(!backing.failed());
    }
    assert!(original.unmap_succeeded());
    assert!(candidate.unmap_succeeded());
    assert!(published.unmap_succeeded());
}

#[test]
/// 取消、另一Doc、旧代次与实际源EIO都撤销原请求；无正文请求不保活映射。
fn cancelled_foreign_and_failed_original_requests_never_issue_an_edit_plan() {
    let (_directory, mut doc) = document(SOURCE);
    let mut cancelled = doc.table_cell_edit_request(0, 1, 0).unwrap();
    cancelled.advance(&doc, false, 4096).unwrap();
    assert_eq!(
        cancelled.advance(&doc, true, 4096),
        Err(MarkdownTableRequestError::Cancelled)
    );
    assert!(cancelled.take_plan_for(&doc).is_err());
    let (_other_directory, other) = document(SOURCE);
    let mut foreign = doc.table_cell_edit_request(0, 1, 0).unwrap();
    assert_eq!(
        foreign.advance(&other, false, 4096),
        Err(MarkdownTableRequestError::SourceSuperseded)
    );
    let original = plan(&doc, 1, 0);
    let (_third_directory, mut third) = document(SOURCE);
    assert!(third.begin_prepared_cell_draft(original).is_err());
    assert_eq!(canonical(&third), SOURCE);
    assert!(!third.has_cell_draft());
    let mut stale = doc.table_cell_edit_request(0, 1, 0).unwrap();
    doc.insert_utf16(0, "新", MarkdownTextSelection::caret(1))
        .unwrap();
    assert_eq!(
        stale.advance(&doc, false, 4096),
        Err(MarkdownTableRequestError::SourceSuperseded)
    );
    doc.undo_text_edit().unwrap();
    let mut failed = doc.table_cell_edit_request(0, 1, 0).unwrap();
    doc.copy_utf16_code_units(0, &mut [0]).unwrap();
    doc.original_snapshot
        .as_ref()
        .unwrap()
        .fail_next_reclaim_for_test();
    assert!(doc.reclaim_original_read_windows_for_test().is_err());
    assert_eq!(
        failed.advance(&doc, true, 4096),
        Err(MarkdownTableRequestError::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    );
    let (_live_directory, live) = document(SOURCE);
    let mut request = live.table_cell_edit_request(0, 1, 0).unwrap();
    request.advance(&live, false, 4096).unwrap();
    let backing = live.original_backing_observer().unwrap();
    drop(live);
    assert!(backing.unmap_succeeded());
    drop(request);
}

#[test]
/// 巨型raw最终只剩小结果时释放匿名文件，有限Inline仍沿一次提交与历史恢复。
fn long_trimmed_input_releases_file_before_retaining_a_small_inline_result() {
    let (_directory, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    let raw = format!("中😀{}", " ".repeat(2 * 1024 * 1024));
    replace(&mut doc, &token, &raw);
    let prepared = doc
        .prepare_cell_draft_commit(doc.capture_cell_draft_edit(&token).unwrap())
        .unwrap();
    assert!(prepared.replacement_backing_observer().is_none());
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    assert!(observer.snapshot().prepared_commit_bytes <= EDIT_SEGMENT_BYTES);
    doc.commit_prepared_cell_draft_observed(prepared, NoopObserver)
        .unwrap()
        .unwrap();
    let expected = SOURCE.replacen("| a |", "| 中😀 |", 1);
    assert_eq!(canonical(&doc), expected);
    doc.undo_text_edit().unwrap();
    assert_eq!(canonical(&doc), SOURCE);
    doc.redo_text_edit().unwrap();
    assert_eq!(canonical(&doc), expected);
    assert!(observer.is_released());
}
