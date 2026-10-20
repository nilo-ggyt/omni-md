//! 真实raw/candidate文件的回退、最后Reader与发布归属；不以owner数字代替实际VM结果。

use super::*;
use crate::markdown_document_buffer::MarkdownTableCellEditStep;
use crate::markdown_document_buffer::changes::NoopObserver;
use crate::markdown_document_buffer::piece_tree::MarkdownEditSegment;
use crate::{MarkdownDocumentBuffer, MarkdownTextSelection};

const SOURCE: &str = "| H | C |\n| --- | --- |\n| a | b |\n";

/// 每次直接从真实文件取得原格计划；长格也不借整行预览或手造端点。
fn begin(document: &mut MarkdownDocumentBuffer) -> MarkdownCellDraft {
    let mut request = document.table_cell_edit_request(0, 1, 0).unwrap();
    let plan = loop {
        match request.advance(document, false, 4096).unwrap() {
            MarkdownTableCellEditStep::Progress { .. } => {}
            MarkdownTableCellEditStep::Ready { .. } => {
                break request.take_plan_for(document).unwrap().unwrap();
            }
            other => panic!("真实格计划没有完成：{other:?}"),
        }
    };
    document.begin_prepared_cell_draft(plan).unwrap()
}

/// 测试只保一个实际Vault文件；临时目录由测试owner退出后删除，不进入产品路径。
fn document() -> (tempfile::TempDir, MarkdownDocumentBuffer) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.md");
    std::fs::write(&path, SOURCE).unwrap();
    let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap();
    (directory, document)
}

/// 原输入API执行一次全格替换；产品仍分4KiB保存文件，本地字符串只是明确的测试输入。
fn replace(document: &mut MarkdownDocumentBuffer, token: &MarkdownCellDraft, text: &str) {
    let range = 0..document.cell_draft_facts(token).unwrap().utf16_len;
    let edit = document.capture_cell_draft_edit(token).unwrap();
    document
        .replace_cell_draft(
            edit,
            range,
            text,
            MarkdownTextSelection::caret(text.encode_utf16().count()),
            None,
        )
        .unwrap();
}

#[test]
/// 无效选区在append之后回退真实文件；后续有效输入、取消及两Reader退场仍核同一释放票。
fn rollback_and_cancel_wait_for_actual_raw_mapping_release() {
    let (_directory, mut document) = document();
    let token = begin(&mut document);
    let resources = document.cell_draft_resource_observer(&token).unwrap();
    let before = document.cell_draft_facts(&token).unwrap();
    let text = "x".repeat(80_000);
    let edit = document.capture_cell_draft_edit(&token).unwrap();
    assert!(matches!(
        document.replace_cell_draft(
            edit,
            0..1,
            &text,
            MarkdownTextSelection::caret(text.len() + 1),
            None,
        ),
        Err(MarkdownCellDraftError::Document(
            MarkdownDocumentBufferError::RangeOutsideDocument
        ))
    ));
    let rollback = resources
        .scope
        .raw_backings
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("append失败前必须已取得本次真实文件释放票");
    assert!(rollback.unmap_succeeded());
    assert_eq!(rollback.mapped_bytes(), 0);
    assert!(!rollback.failed());
    assert_eq!(document.cell_draft_facts(&token).unwrap(), before);
    replace(&mut document, &token, &text);
    let raw = resources
        .scope
        .raw_backings
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    let first = document.capture_cell_draft(&token).unwrap();
    let second = document.capture_cell_draft(&token).unwrap();
    assert!(document.cancel_cell_draft(&token));
    assert_eq!(raw.mapped_bytes(), text.len());
    assert!(!resources.is_released());
    drop(first);
    assert!(!raw.unmap_succeeded()); // 第二个实际读取payload仍拥有同一映射。
    drop(second);
    assert!(raw.unmap_succeeded());
    assert_eq!(raw.mapped_bytes(), 0);
    assert!(resources.is_released());
    assert_eq!(resources.snapshot().first_raw_cleanup_errno, None);
    assert!(document.drain_cancelled_cell_draft());
}

#[test]
/// raw真正驱逐失败即使最后munmap成功也保留原errno；释放结论不能被清空owner数字洗掉。
fn raw_io_failure_survives_branch_and_last_reader_drop() {
    let (_directory, mut document) = document();
    let token = begin(&mut document);
    replace(&mut document, &token, &"x".repeat(80_000));
    let resources = document.cell_draft_resource_observer(&token).unwrap();
    let reader = document.capture_cell_draft(&token).unwrap();
    let pending = document.active_cell_draft(&token).unwrap();
    let MarkdownEditSegment::Frozen(backing) = pending.edits.segments.last().unwrap().as_ref()
    else {
        panic!("真实大输入必须进入raw文件段");
    };
    let mapping = backing.observer();
    // 注入沿既有真实msync成功后的EIO机制；设置flag本身不撤销Source或制造释放结果。
    backing.fail_next_reclaim_for_test();
    assert_eq!(
        backing.reclaim_unpinned_for_test(),
        Err(crate::original_backing::OriginalBackingError::Unavailable(
            libc::EIO
        ))
    );
    assert!(document.cell_draft_facts(&token).is_err());
    assert!(document.cancel_cell_draft(&token));
    drop(reader);
    assert!(mapping.unmap_succeeded());
    assert_eq!(mapping.first_errno(), Some(libc::EIO));
    assert_eq!(
        resources.snapshot().first_raw_cleanup_errno,
        Some(libc::EIO)
    );
    assert!(!resources.is_released());
    assert!(!document.drain_cancelled_cell_draft()); // 实际失败票不能作为健康retired槽排空。
}

#[test]
/// 成功发布只移交实际Frozen；后继草稿继承它时不重复承担canonical文件释放义务。
fn published_and_inherited_backings_remain_document_owned() {
    let (_directory, mut document) = document();
    let token = begin(&mut document);
    replace(&mut document, &token, &"x".repeat(80_000));
    let resources = document.cell_draft_resource_observer(&token).unwrap();
    let prepared = document
        .prepare_cell_draft_commit(document.capture_cell_draft_edit(&token).unwrap(), None)
        .unwrap();
    let mapping = prepared.replacement_backing_observer().unwrap();
    document
        .commit_prepared_cell_draft_observed(prepared, NoopObserver)
        .unwrap()
        .edit
        .unwrap();
    assert!(resources.is_released()); // raw与未发布候选的义务均结束，Doc仍合法拥有最终文件。
    assert_eq!(mapping.mapped_bytes(), 80_000);
    let next = begin(&mut document);
    let inherited = document.cell_draft_resource_observer(&next).unwrap();
    assert!(document.cancel_cell_draft(&next));
    assert!(inherited.is_released());
    assert!(!mapping.unmap_succeeded());
    document.undo_text_edit().unwrap();
    document.redo_text_edit().unwrap();
    drop(document);
    assert!(mapping.unmap_succeeded());
    assert!(!mapping.failed());
}

#[test]
/// 长raw缩回有限Inline后，seal的临时映射已实际释放，不能等publish才错误地转归Doc。
fn trimmed_inline_candidate_finishes_its_file_obligation_before_publish() {
    let (_directory, mut document) = document();
    let token = begin(&mut document);
    replace(
        &mut document,
        &token,
        &format!("{}正文", " ".repeat(80_000)),
    );
    let resources = document.cell_draft_resource_observer(&token).unwrap();
    let prepared = document
        .prepare_cell_draft_commit(document.capture_cell_draft_edit(&token).unwrap(), None)
        .unwrap();
    assert!(prepared.replacement_backing_observer().is_none());
    assert!(document.cancel_cell_draft(&token));
    drop(prepared);
    assert!(resources.is_released());
    assert_eq!(resources.snapshot().first_candidate_cleanup_errno, None);
}

#[test]
/// 小文件候选即使真实munmap成功，先前驱逐EIO仍使释放失败；原raw输入权限保持健康。
fn small_candidate_io_failure_retains_errno_without_poisoning_raw() {
    let (_directory, mut document) = document();
    let token = begin(&mut document);
    let resources = document.cell_draft_resource_observer(&token).unwrap();
    let mut claim = resources.scope.reserve_commit().unwrap();
    let mut file = crate::immutable_snapshot::CanonicalSnapshotOutput::new().unwrap();
    file.write_all("正文".as_bytes()).unwrap();
    let backing = file.freeze().unwrap().unwrap();
    let mapping = backing.observer();
    claim.observe_backing(mapping.clone());
    let lease = backing.lease(0..backing.len()).unwrap();
    lease.finish().unwrap();
    mapping.fail_next_reclaim_for_test();
    assert_eq!(
        backing.reclaim_unpinned_for_test(),
        Err(crate::original_backing::OriginalBackingError::Unavailable(
            libc::EIO
        ))
    );
    drop(backing); // 与seal缩回Inline失败相同：先结束唯一映射，再归还候选claim。
    drop(claim);
    assert!(mapping.unmap_succeeded());
    assert_eq!(mapping.mapped_bytes(), 0);
    assert_eq!(
        resources.snapshot().first_candidate_cleanup_errno,
        Some(libc::EIO)
    );
    assert_eq!(resources.snapshot().first_source_errno, None);
    replace(&mut document, &token, "仍可编辑");
    assert!(document.cancel_cell_draft(&token));
    assert!(!resources.is_released());
}
