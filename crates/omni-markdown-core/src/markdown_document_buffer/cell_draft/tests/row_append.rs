//! 原草稿与真实表尾的组合提交；原nonce、有限源范围和一次canonical Undo共同认证结果。

use super::*;
use crate::markdown_document_buffer::changes::MarkdownObservedEditError;
use crate::markdown_viewport_presentation::table_window::TABLE_WINDOW_MAX_BYTES;

/// 由真实RequiredRow续体获得同源窗口；测试不能手造VerifiedTableEnd绕过源代次。
fn row_window(doc: &MarkdownDocumentBuffer, row: usize) -> VerifiedMarkdownTableWindow {
    let mut request = doc
        .table_window_request(
            MarkdownTableWindowTarget::RequiredRow {
                header_byte_start: 0,
                row_ordinal: row,
            },
            0..0,
            1,
            usize::MAX..usize::MAX,
        )
        .unwrap();
    for _ in 0..doc.byte_len().saturating_mul(64).saturating_add(10_000) {
        // 沿实际结构请求取得End，不按最后一个可见row或夹具行数构造终点证明。
        match request.advance(doc, false, 4096).unwrap() {
            MarkdownTableWindowStep::Progress { .. } => {}
            MarkdownTableWindowStep::Ready { .. } => {
                return request.take_ready_for(doc).unwrap().unwrap();
            }
            other => panic!("真实末行窗口没有Ready：{other:?}"),
        }
    }
    panic!("有限合成末行请求未结束");
}

/// 仅转交当前草稿原编辑胶囊和真实窗口，测试不直接构造PreparedCommit。
fn prepare_append(
    doc: &MarkdownDocumentBuffer,
    token: &MarkdownCellDraft,
    window: &VerifiedMarkdownTableWindow,
    row: usize,
    column: usize,
) -> Result<MarkdownCellDraftPreparedCommit, MarkdownCellDraftError> {
    doc.prepare_cell_draft_row_append(doc.capture_cell_draft_edit(token)?, window, row, column)
}

/// 固定小样本核对精确字节、唯一Undo/Redo和后继真实行成员；oracle不进入产品。
fn single_append_history(
    source: &str,
    row: usize,
    column: usize,
    raw: Option<&str>,
    expected: &str,
) {
    let (_directory, mut doc) = document(source);
    let window = row_window(&doc, row);
    assert!(window.table_end_for(&doc).unwrap().is_some());
    let token = doc.begin_cell_draft(&window, row, column).unwrap();
    let baseline = doc.current_revision().unwrap();
    let selection = doc.selection();
    if let Some(raw) = raw {
        replace(&mut doc, &token, raw);
    }
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let before = doc.storage_profile();
    let prepared = prepare_append(&doc, &token, &window, row, column).unwrap();
    assert_eq!(prepared.next_cell(), Some((row + 1, 0)));
    assert_eq!(doc.storage_profile(), before);
    assert!(observer.snapshot().prepared_commit_bytes <= TABLE_WINDOW_MAX_BYTES);
    doc.commit_prepared_cell_draft_observed(prepared, NoopObserver)
        .unwrap()
        .unwrap();
    assert_eq!(doc.generation(), 1);
    assert_eq!(doc.storage_profile().undo_entries, before.undo_entries + 1);
    assert_eq!(canonical(&doc), expected);
    assert!(observer.is_released());
    assert!(!doc.has_cell_draft());
    assert!(window.presentation_for(&doc).is_err()); // 一次真正publish撤销旧窗口，不让后继格消费旧End。
    doc.undo_text_edit().unwrap();
    assert_eq!(doc.current_revision().unwrap(), baseline);
    assert_eq!(canonical(&doc), source);
    assert_eq!(doc.selection(), selection);
    assert!(!doc.can_undo());
    doc.redo_text_edit().unwrap();
    assert_eq!(canonical(&doc), expected);
    let next = row_window(&doc, row + 1);
    assert!(
        next.presentation_for(&doc)
            .unwrap()
            .cell(row + 1, 0)
            .is_some()
    );
}

#[test]
/// 同一raw草稿在LF、CRLF、引用、列表和无末EOL场景保留全部表外字节。
fn final_raw_draft_and_real_empty_row_publish_once_with_original_eol_and_container() {
    for (source, row, expected) in [
        (
            "| H | C |\n| - | - |\n| a | b |\n",
            1,
            "| H | C |\n| - | - |\n| a | 新\\|中 😀 |\n| | |\n",
        ),
        (
            "| H | C |\r\n| - | - |\r\n| a | b |\r\n\r\n后继正文😀\r\n",
            1,
            "| H | C |\r\n| - | - |\r\n| a | 新\\|中 😀 |\r\n| | |\r\n\r\n后继正文😀\r\n",
        ),
        (
            "> > | H | C |\r\n> > | - | - |\r\n> > | a | b |",
            1,
            "> > | H | C |\r\n> > | - | - |\r\n> > | a | 新\\|中 😀 |\r\n> > | | |",
        ),
        (
            "3. | H | C |\r\n   | - | - |",
            0,
            "3. | H | 新\\|中 😀 |\r\n   | - | - |\r\n   | | |",
        ),
    ] {
        // 原raw输入的pipe/CRLF/Unicode只sanitize一次；末行后真实段落和EOF形式都保留。
        single_append_history(source, row, 1, Some("  新|中\r\n😀  "), expected);
    }
}

#[test]
/// 共享空格锚点只补一次分隔符；原格未改也必须将新增真实行纳入一次Undo。
fn implicit_last_cell_prefix_is_added_once_and_unchanged_cell_still_appends_one_undo() {
    single_append_history(
        "| H1 | H2 | H3 | H4 |\n| - | - | - | - |\n|a|\n",
        1,
        3,
        Some("新😀"),
        "| H1 | H2 | H3 | H4 |\n| - | - | - | - |\n|a| |  | 新😀\n| | | | |\n",
    );
    single_append_history(
        SOURCE,
        1,
        1,
        None,
        "| H | C |\n| --- | --- |\n| a | b |\n| | |\n",
    );
    single_append_history(
        "| H |\n| - |\n| a |",
        1,
        0,
        None,
        "| H |\n| - |\n| a |\n| |",
    );
}

#[test]
/// 两份真实Reader存在时只允许准备，失败提交消费准入票且保持可读原草稿。
fn append_admission_keeps_two_readers_and_commit_waits_for_their_actual_release() {
    let (_directory, mut doc) = document(SOURCE);
    let window = row_window(&doc, 1);
    let token = doc.begin_cell_draft(&window, 1, 1).unwrap();
    replace(&mut doc, &token, "候选");
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let first = doc.capture_cell_draft(&token).unwrap();
    let second = doc.capture_cell_draft(&token).unwrap();
    let facts = doc.cell_draft_facts(&token).unwrap();
    let baseline = doc.storage_profile();
    let prepared = prepare_append(&doc, &token, &window, 1, 1).unwrap();
    assert_eq!(observer.snapshot().read_roots, 2);
    assert!(matches!(
        prepare_append(&doc, &token, &window, 1, 1),
        Err(MarkdownCellDraftError::CommitBusy)
    ));
    assert!(matches!(
        doc.commit_prepared_cell_draft_observed(prepared, NoopObserver),
        Err(MarkdownCellDraftCommitError::Draft(
            MarkdownCellDraftError::ReadersBusy
        ))
    ));
    assert_eq!(doc.cell_draft_facts(&token).unwrap(), facts);
    assert_eq!(doc.storage_profile(), baseline);
    assert_eq!(read(&first), "候选");
    assert_eq!(read(&second), "候选");
    assert!(!observer.snapshot().prepared_commit_live);
    // 失败的票已消费；原两份reader先真实drop，再从仍相同serial的原草稿准备下一份唯一票。
    drop((first, second));
    let prepared = prepare_append(&doc, &token, &window, 1, 1).unwrap();
    doc.commit_prepared_cell_draft_observed(prepared, NoopObserver)
        .unwrap()
        .unwrap();
    assert_eq!(
        canonical(&doc),
        "| H | C |\n| --- | --- |\n| a | 候选 |\n| | |\n"
    );
    assert!(observer.is_released());
}

#[test]
/// 窗口末行、其他真实行和共享锚点的另一缺格都不能冒充原末格权限。
fn partial_end_wrong_column_or_implicit_coordinate_never_retargets_the_original_draft() {
    let source = "| H | C |\n| - | - |\n| a | b |\n| c | d |\n";
    let (_directory, mut doc) = document(source);
    let partial = row_window(&doc, 1);
    assert!(partial.table_end_for(&doc).unwrap().is_none());
    let token = doc.begin_cell_draft(&partial, 1, 1).unwrap();
    replace(&mut doc, &token, "原草稿");
    let before = doc.cell_draft_facts(&token).unwrap();
    assert!(matches!(
        prepare_append(&doc, &token, &partial, 1, 1),
        Err(MarkdownCellDraftError::Table(
            MarkdownTableWindowError::ContextPending
        ))
    ));
    let end = row_window(&doc, 2);
    assert!(prepare_append(&doc, &token, &end, 2, 1).is_err());
    assert_eq!(doc.cell_draft_facts(&token).unwrap(), before);
    assert_eq!(canonical(&doc), source);
    assert!(doc.cancel_cell_draft(&token));

    let source = "| A | B | C | D |\n| - | - | - | - |\n|a|\n";
    let (_directory, mut doc) = document(source);
    let end = row_window(&doc, 1);
    let token = doc.begin_cell_draft(&end, 1, 2).unwrap();
    let before = doc.cell_draft_facts(&token).unwrap();
    // 缺格2/3共享零长插入range，原prefix不同仍必须拒绝，不能把column3当原格。
    assert!(matches!(
        prepare_append(&doc, &token, &end, 1, 3),
        Err(MarkdownCellDraftError::Table(
            MarkdownTableWindowError::InvalidSource
        ))
    ));
    assert!(prepare_append(&doc, &token, &end, 1, 2).is_err()); // 真实末列门不能因原格匹配而放宽。
    assert_eq!(doc.cell_draft_facts(&token).unwrap(), before);
    assert_eq!(canonical(&doc), source);
    assert!(doc.cancel_cell_draft(&token));
}

#[test]
/// 原serial、组合输入、Doc身份与取消分别撤权，任何拒绝都不得发布半条源替换。
fn serial_composition_foreign_source_and_cancelled_append_tickets_keep_the_raw_branch() {
    for action in 0..5 {
        let (_directory, mut doc) = document(SOURCE);
        let window = row_window(&doc, 1);
        let token = doc.begin_cell_draft(&window, 1, 1).unwrap();
        replace(&mut doc, &token, "原始|草稿");
        let observer = doc.cell_draft_resource_observer(&token).unwrap();
        match action {
            0 => {
                // 只移动草稿选择也推进输入serial；旧准入不能套到后来选择上。
                let prepared = prepare_append(&doc, &token, &window, 1, 1).unwrap();
                let edit = doc.capture_cell_draft_edit(&token).unwrap();
                doc.set_cell_draft_selection(edit, MarkdownTextSelection::caret(0))
                    .unwrap();
                assert!(matches!(
                    doc.commit_prepared_cell_draft_observed(prepared, NoopObserver),
                    Err(MarkdownCellDraftCommitError::Draft(
                        MarkdownCellDraftError::Stale
                    ))
                ));
            }
            1 => {
                // 活动组合范围保持原值，不允许准备动作暗中确认IME。
                edit(
                    &mut doc,
                    &token,
                    0..0,
                    "",
                    MarkdownTextSelection::caret(0),
                    Some(0..1),
                )
                .unwrap();
                assert!(matches!(
                    prepare_append(&doc, &token, &window, 1, 1),
                    Err(MarkdownCellDraftError::CompositionActive)
                ));
            }
            2 => {
                // 内容与代次相同的另一Doc窗口仍无权描述原草稿末格。
                let (_other_directory, other) = document(SOURCE);
                let foreign = row_window(&other, 1);
                assert!(matches!(
                    prepare_append(&doc, &token, &foreign, 1, 1),
                    Err(MarkdownCellDraftError::Table(
                        MarkdownTableWindowError::InvalidSource
                    ))
                ));
            }
            3 => {
                // 反方向也拒绝：另一Doc不能消费已经准备好的原草稿票。
                let prepared = prepare_append(&doc, &token, &window, 1, 1).unwrap();
                let (_other_directory, mut other) = document(SOURCE);
                let other_window = row_window(&other, 1);
                let other_token = other.begin_cell_draft(&other_window, 1, 1).unwrap();
                assert!(matches!(
                    other.commit_prepared_cell_draft_observed(prepared, NoopObserver),
                    Err(MarkdownCellDraftCommitError::Draft(
                        MarkdownCellDraftError::Stale
                    ))
                ));
                assert_eq!(canonical(&other), SOURCE);
                assert!(other.cancel_cell_draft(&other_token));
            }
            _ => {
                // 取消先结束分支，最后的有限候选仍依真实Drop归还原claim。
                let prepared = prepare_append(&doc, &token, &window, 1, 1).unwrap();
                assert!(doc.cancel_cell_draft(&token));
                assert!(!observer.is_released()); // 原有限候选仍须先真实归还，取消不伪造0字节收据。
                assert!(matches!(
                    doc.commit_prepared_cell_draft_observed(prepared, NoopObserver),
                    Err(MarkdownCellDraftCommitError::Draft(
                        MarkdownCellDraftError::Stale
                    ))
                ));
                assert!(observer.is_released());
                assert!(doc.drain_cancelled_cell_draft());
            }
        }
        assert_eq!(canonical(&doc), SOURCE);
        assert_eq!(doc.generation(), 0);
        assert!(!doc.can_undo());
        assert!(!observer.snapshot().prepared_commit_live);
        if action != 4 {
            assert_eq!(read(&doc.capture_cell_draft(&token).unwrap()), "原始|草稿");
            assert!(doc.cancel_cell_draft(&token));
        }
    }
}

struct Reject;
impl MarkdownTextChangeObserver for Reject {
    type Output = ();
    type Error = &'static str;
    /// 接收原有限变化；本例专门让finish拒绝实际候选，而非跳过真实准备过程。
    fn changed(&mut self, _: MarkdownTextChange) -> Result<(), Self::Error> {
        Ok(())
    }
    /// 原候选完整建立后拒绝，迫使生产路径执行真实rollback。
    fn finish(
        self,
        _: MarkdownPreparedText<'_>,
        _: MarkdownTextSelection,
    ) -> Result<(), Self::Error> {
        Err("reject combined row append")
    }
}

#[test]
/// 原观察者拒绝时回退canonical候选与日志，并继续保留同一草稿的本地Undo。
fn rejected_observer_rolls_back_combined_candidate_and_keeps_original_local_undo() {
    let (_directory, mut doc) = document(SOURCE);
    let window = row_window(&doc, 1);
    let token = doc.begin_cell_draft(&window, 1, 1).unwrap();
    replace(&mut doc, &token, "第一稿");
    replace(&mut doc, &token, "最终|草稿");
    let before = doc.cell_draft_facts(&token).unwrap();
    let baseline = doc.storage_profile();
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let prepared = prepare_append(&doc, &token, &window, 1, 1).unwrap();
    assert!(matches!(
        doc.commit_prepared_cell_draft_observed(prepared, Reject),
        Err(MarkdownCellDraftCommitError::Edit(
            MarkdownObservedEditError::Observer("reject combined row append")
        ))
    ));
    assert!(!doc.has_prepared_text_input());
    assert_eq!(doc.storage_profile(), baseline);
    assert_eq!(doc.cell_draft_facts(&token).unwrap(), before);
    assert_eq!(canonical(&doc), SOURCE);
    assert!(!observer.snapshot().prepared_commit_live);
    let edit = doc.capture_cell_draft_edit(&token).unwrap();
    doc.restore_cell_draft_history(edit, MarkdownCellDraftHistoryDirection::Undo)
        .unwrap()
        .unwrap();
    assert_eq!(read(&doc.capture_cell_draft(&token).unwrap()), "第一稿");
    assert!(doc.cancel_cell_draft(&token));
}

#[test]
/// 取消不能遮住真实映射回收EIO；错误返回后唯一有限String必须已归还。
fn real_source_error_precedes_stale_cancelled_combined_admission_and_releases_its_string() {
    let (_directory, mut doc) = document(SOURCE);
    let window = row_window(&doc, 1);
    let token = doc.begin_cell_draft(&window, 1, 1).unwrap();
    replace(&mut doc, &token, "候选");
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let prepared = prepare_append(&doc, &token, &window, 1, 1).unwrap();
    assert!(doc.cancel_cell_draft(&token));
    doc.copy_utf16_code_units(0, &mut [0]).unwrap();
    doc.original_snapshot
        .as_ref()
        .unwrap()
        .fail_next_reclaim_for_test();
    assert_eq!(
        doc.reclaim_original_read_windows_for_test(),
        Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
            libc::EIO
        ))
    );
    assert!(matches!(
        doc.commit_prepared_cell_draft_observed(prepared, NoopObserver),
        Err(MarkdownCellDraftCommitError::Draft(
            MarkdownCellDraftError::Document(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
            )
        ))
    ));
    assert!(!observer.snapshot().prepared_commit_live);
    assert_eq!(observer.snapshot().first_source_errno, Some(libc::EIO));
    assert!(observer.is_released());
}

#[test]
/// 五千行真实表只签末格附近替换，候选容量不随前部行数增长。
fn five_thousand_row_source_keeps_only_final_cell_and_bounded_append_candidate() {
    let mut source = String::from("| H | C |\n| - | - |\n");
    for row in 1..=5000 {
        source.push_str(&format!("| {row} | 中😀 |\n"));
    }
    let (_directory, mut doc) = document(&source);
    let window = row_window(&doc, 5000);
    assert_eq!(window.presentation_for(&doc).unwrap().rows.len(), 1);
    let before = doc.current_revision().unwrap();
    let token = doc.begin_cell_draft(&window, 5000, 1).unwrap();
    replace(&mut doc, &token, "末格");
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let prepared = prepare_append(&doc, &token, &window, 5000, 1).unwrap();
    assert_eq!(prepared.next_cell(), Some((5001, 0)));
    assert!(observer.snapshot().prepared_commit_bytes < 64);
    let original_units = doc.utf16_len();
    let (edit, ()) = doc
        .commit_prepared_cell_draft_observed(prepared, NoopObserver)
        .unwrap()
        .unwrap();
    assert!(edit.replaced_utf16_range.start > original_units - 16);
    assert!(edit.replaced_utf16_range.end <= original_units);
    let end = row_window(&doc, 5001);
    assert_eq!(
        end.table_end_for(&doc)
            .unwrap()
            .unwrap()
            .last_body_row_ordinal(),
        Some(5001)
    );
    doc.undo_text_edit().unwrap();
    assert_eq!(doc.current_revision().unwrap(), before);
    assert!(!doc.can_undo());
}

#[test]
/// 长末格使用真实封存候选，两Scene仍活时消费失败真正unmap，归还后一次追加与Undo。
fn long_raw_append_preserves_two_readers_and_publishes_one_frozen_transaction() {
    let (_directory, mut doc) = document(SOURCE);
    let window = row_window(&doc, 1);
    let token = doc.begin_cell_draft(&window, 1, 1).unwrap();
    replace(&mut doc, &token, &"x".repeat(2 * 1024 * 1024));
    let first = doc.capture_cell_draft(&token).unwrap();
    let second = doc.capture_cell_draft(&token).unwrap();
    let before = doc.cell_draft_facts(&token).unwrap();
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let prepared = prepare_append(&doc, &token, &window, 1, 1).unwrap();
    assert_eq!(prepared.next_cell(), Some((2, 0)));
    let candidate = prepared.replacement_backing_observer().unwrap();
    assert_eq!(doc.cell_draft_facts(&token).unwrap(), before);
    assert_eq!(observer.snapshot().read_roots, 2);
    assert_eq!(observer.snapshot().peak_prepared_commit_bytes, 0);
    assert_eq!(doc.generation(), 0);
    assert!(matches!(
        doc.commit_prepared_cell_draft_observed(prepared, NoopObserver),
        Err(MarkdownCellDraftCommitError::Draft(
            MarkdownCellDraftError::ReadersBusy
        ))
    ));
    assert!(candidate.unmap_succeeded());
    drop((first, second));
    let prepared = prepare_append(&doc, &token, &window, 1, 1).unwrap();
    doc.commit_prepared_cell_draft_observed(prepared, NoopObserver)
        .unwrap()
        .unwrap();
    let expected = SOURCE.replacen(
        "| a | b |\n",
        &format!("| a | {} |\n| | |\n", "x".repeat(2 * 1024 * 1024)),
        1,
    );
    assert_eq!(canonical(&doc), expected);
    assert_eq!(doc.storage_profile().undo_entries, 1);
    doc.undo_text_edit().unwrap();
    assert_eq!(canonical(&doc), SOURCE);
    doc.redo_text_edit().unwrap();
    assert_eq!(canonical(&doc), expected);
    assert!(observer.is_released());
}

#[test]
/// 原Present范围排除尾padding；大尾部必须从同一认证End逐片保留，不能误报旧64Ki复制超限。
fn long_original_padding_is_streamed_with_exact_eol_and_one_undo() {
    for eol in ["\n", "\r\n", "\r", ""] {
        let padding = " \t".repeat(40_000);
        let source = format!("| H | C |\n| --- | --- |\n| a | 原{padding}|{eol}");
        let expected = format!(
            "| H | C |\n| --- | --- |\n| a | 改{padding}|{}| | |{}",
            if eol.is_empty() { "\n" } else { eol },
            eol
        );
        let (_directory, mut doc) = document(&source);
        // 保留原80,000 ASCII padding；由真实4KiB原格请求进入，不能换夹具或伪造Window。
        let target = super::original_source::plan(&doc, 1, 1);
        let token = doc.begin_prepared_cell_draft(target).unwrap();
        replace(&mut doc, &token, "改");
        let prepared = doc
            .prepare_cell_draft_original_row_append(doc.capture_cell_draft_edit(&token).unwrap())
            .unwrap();
        assert_eq!(prepared.next_cell(), Some((2, 0)));
        doc.commit_prepared_cell_draft_observed(prepared, NoopObserver)
            .unwrap()
            .unwrap();
        assert_eq!(canonical(&doc), expected);
        assert_eq!(doc.storage_profile().undo_entries, 1);
        doc.undo_text_edit().unwrap();
        assert_eq!(canonical(&doc), source);
        doc.redo_text_edit().unwrap();
        assert_eq!(canonical(&doc), expected);
    }
}

#[test]
/// 大raw清洗为空仍可沿原末格追加新行；不能将空候选误判为取消整次Tab。
fn long_whitespace_cell_and_new_row_publish_as_one_original_transaction() {
    let raw = "\u{2003}\r\n\t".repeat(40_000);
    single_append_history(
        SOURCE,
        1,
        1,
        Some(&raw),
        "| H | C |\n| --- | --- |\n| a |  |\n| | |\n",
    );
}

#[test]
/// Undo回到相同字节也不复活原End；当前草稿只能使用重新取得的真实代次窗口。
fn old_generation_end_does_not_reauthorize_current_draft_after_canonical_undo() {
    let (_directory, mut doc) = document(SOURCE);
    let old_window = row_window(&doc, 1);
    doc.insert_utf16(0, "改", MarkdownTextSelection::caret(1))
        .unwrap();
    doc.undo_text_edit().unwrap();
    let current = row_window(&doc, 1);
    let token = doc.begin_cell_draft(&current, 1, 1).unwrap();
    replace(&mut doc, &token, "候选");
    let before = doc.cell_draft_facts(&token).unwrap();
    let baseline = doc.storage_profile();
    assert!(matches!(
        prepare_append(&doc, &token, &old_window, 1, 1),
        Err(MarkdownCellDraftError::Table(
            MarkdownTableWindowError::InvalidSource
        ))
    ));
    assert_eq!(doc.cell_draft_facts(&token).unwrap(), before);
    assert_eq!(doc.storage_profile(), baseline);
    assert_eq!(canonical(&doc), SOURCE);
    assert!(doc.cancel_cell_draft(&token));
}
