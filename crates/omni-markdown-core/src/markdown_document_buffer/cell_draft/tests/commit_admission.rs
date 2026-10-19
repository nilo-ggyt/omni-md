//! 提交先准入再撤Scene；只消费原serial的一份有限sanitize，不把后继草稿重新补签。

use super::*;
use crate::markdown_document_buffer::changes::MarkdownObservedEditError;

fn prepare(
    doc: &MarkdownDocumentBuffer,
    token: &MarkdownCellDraft,
) -> Result<MarkdownCellDraftPreparedCommit, MarkdownCellDraftError> {
    doc.prepare_cell_draft_commit(doc.capture_cell_draft_edit(token)?)
}

fn assert_no_admission(observer: &MarkdownCellDraftResourceObserver) {
    let facts = observer.snapshot();
    assert!(!facts.prepared_commit_live);
    assert_eq!(facts.prepared_commit_bytes, 0);
}

struct Reject;
impl MarkdownTextChangeObserver for Reject {
    type Output = ();
    type Error = &'static str;

    fn changed(&mut self, _: MarkdownTextChange) -> Result<(), Self::Error> {
        Ok(())
    }

    fn finish(
        self,
        _: MarkdownPreparedText<'_>,
        _: MarkdownTextSelection,
    ) -> Result<(), Self::Error> {
        Err("reject admission publish")
    }
}

#[test]
fn long_admission_keeps_two_real_scenes_and_busy_consumption_unmaps_only_its_candidate() {
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(&mut doc, &token, &"x".repeat(2 * 1024 * 1024));
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let before = doc.cell_draft_facts(&token).unwrap();
    let baseline = doc.storage_profile();
    let first = doc.capture_cell_draft(&token).unwrap();
    let second = doc.capture_cell_draft(&token).unwrap();
    let prepared = prepare(&doc, &token).unwrap();
    let candidate = prepared.replacement_backing_observer().unwrap();
    assert_eq!(candidate.mapped_bytes(), 2 * 1024 * 1024);
    assert_eq!(doc.cell_draft_facts(&token).unwrap(), before);
    assert_eq!(doc.storage_profile(), baseline);
    assert_eq!(observer.snapshot().read_roots, 2);
    assert_eq!(observer.snapshot().peak_prepared_commit_bytes, 0);
    assert!(observer.snapshot().prepared_commit_live);
    for scene in [&first, &second] {
        let mut tail = [0; 4];
        scene
            .copy_utf16_code_units(&scene.authority(), before.utf16_len - 4, &mut tail)
            .unwrap();
        assert_eq!(tail, [u16::from(b'x'); 4]);
    }
    // 兼容入口的原Reader优先级保持；新prepare没有先拆Scene以躲过ReadersBusy。
    assert!(matches!(
        doc.commit_cell_draft_observed(&token, NoopObserver),
        Err(MarkdownCellDraftCommitError::Draft(
            MarkdownCellDraftError::ReadersBusy
        ))
    ));
    assert_eq!(canonical(&doc), SOURCE);
    assert!(matches!(
        doc.commit_prepared_cell_draft_observed(prepared, NoopObserver),
        Err(MarkdownCellDraftCommitError::Draft(
            MarkdownCellDraftError::ReadersBusy
        ))
    ));
    assert!(candidate.unmap_succeeded());
    assert_no_admission(&observer);
    assert_eq!(observer.snapshot().read_roots, 2);
    drop((first, second));
    let prepared = prepare(&doc, &token).unwrap();
    doc.commit_prepared_cell_draft_observed(prepared, NoopObserver)
        .unwrap()
        .unwrap();
    let expected = SOURCE.replacen("| a |", &format!("| {} |", "x".repeat(2 * 1024 * 1024)), 1);
    assert_eq!(canonical(&doc), expected);
    assert_eq!(
        doc.storage_profile().undo_entries,
        baseline.undo_entries + 1
    );
    doc.undo_text_edit().unwrap();
    assert_eq!(canonical(&doc), SOURCE);
    doc.redo_text_edit().unwrap();
    assert_eq!(canonical(&doc), expected);
    assert!(observer.is_released());
}

#[test]
fn admitted_ticket_with_live_readers_cannot_publish_and_is_really_returned() {
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(&mut doc, &token, "candidate");
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let before = doc.cell_draft_facts(&token).unwrap();
    let baseline = doc.storage_profile();
    let first = doc.capture_cell_draft(&token).unwrap();
    let second = doc.capture_cell_draft(&token).unwrap();
    let prepared = prepare(&doc, &token).unwrap();
    assert!(observer.snapshot().prepared_commit_live);
    assert!(observer.snapshot().prepared_commit_bytes >= "candidate".len());
    assert_eq!(observer.snapshot().read_roots, 2);
    assert!(matches!(
        doc.commit_prepared_cell_draft_observed(prepared, NoopObserver),
        Err(MarkdownCellDraftCommitError::Draft(
            MarkdownCellDraftError::ReadersBusy
        ))
    ));
    assert_no_admission(&observer); // 原票按值消费；失败不保一个隐含String供后继重签。
    assert_eq!(doc.cell_draft_facts(&token).unwrap(), before);
    assert_eq!(doc.storage_profile(), baseline);
    assert_eq!(read(&first), "candidate");
    assert_eq!(read(&second), "candidate");
    assert_eq!(canonical(&doc), SOURCE);
    drop((first, second));
    assert!(doc.cancel_cell_draft(&token));
}

#[test]
fn same_admission_after_scene_drop_publishes_once_with_one_canonical_undo() {
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(&mut doc, &token, "  new|中\r\n😀  ");
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let first = doc.capture_cell_draft(&token).unwrap();
    let second = doc.capture_cell_draft(&token).unwrap();
    let before = doc.storage_profile();
    let prepared = prepare(&doc, &token).unwrap();
    assert_eq!(doc.storage_profile(), before); // 准入没有canonical append、日志或历史发布。
    assert_eq!(doc.generation(), 0);
    drop((first, second));
    doc.commit_prepared_cell_draft_observed(prepared, NoopObserver)
        .unwrap()
        .unwrap();
    let expected = SOURCE.replacen("| a |", "| new\\|中 😀 |", 1);
    assert_eq!(canonical(&doc), expected);
    assert_eq!(doc.generation(), 1);
    assert_eq!(doc.storage_profile().undo_entries, before.undo_entries + 1);
    assert!(!doc.has_cell_draft());
    assert!(observer.is_released());
    assert_no_admission(&observer);
    doc.undo_text_edit().unwrap();
    assert_eq!(canonical(&doc), SOURCE);
    doc.redo_text_edit().unwrap();
    assert_eq!(canonical(&doc), expected);
}

#[test]
fn only_one_live_admission_exists_and_drop_does_not_publish_or_change_draft() {
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(&mut doc, &token, "candidate");
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let before = doc.cell_draft_facts(&token).unwrap();
    let baseline = doc.storage_profile();
    let prepared = prepare(&doc, &token).unwrap();
    let capacity = observer.snapshot().prepared_commit_bytes;
    assert!(matches!(
        prepare(&doc, &token),
        Err(MarkdownCellDraftError::CommitBusy)
    ));
    assert!(matches!(
        doc.commit_cell_draft_observed(&token, NoopObserver),
        Err(MarkdownCellDraftCommitError::Draft(
            MarkdownCellDraftError::CommitBusy
        ))
    ));
    assert_eq!(observer.snapshot().prepared_commit_bytes, capacity);
    assert_eq!(doc.cell_draft_facts(&token).unwrap(), before);
    assert_eq!(doc.storage_profile(), baseline);
    drop(prepared);
    assert_no_admission(&observer);
    let next = prepare(&doc, &token).unwrap();
    assert_eq!(observer.snapshot().prepared_commit_bytes, capacity);
    drop(next);
    assert_eq!(doc.cell_draft_facts(&token).unwrap(), before);
    assert_eq!(canonical(&doc), SOURCE);
    assert!(doc.cancel_cell_draft(&token));
}

#[test]
fn real_edit_selection_and_unmark_cannot_reauthorize_the_old_admission() {
    for change in 0..3 {
        let (_dir, mut doc) = document(SOURCE);
        let token = begin(&mut doc);
        replace(&mut doc, &token, "candidate");
        let observer = doc.cell_draft_resource_observer(&token).unwrap();
        let prepared = prepare(&doc, &token).unwrap();
        match change {
            0 => {
                edit(
                    &mut doc,
                    &token,
                    0..1,
                    "X",
                    MarkdownTextSelection::caret(1),
                    None,
                )
                .unwrap();
            }
            1 => {
                let edit = doc.capture_cell_draft_edit(&token).unwrap();
                doc.set_cell_draft_selection(edit, MarkdownTextSelection::caret(0))
                    .unwrap();
            }
            _ => {
                // 空字符操作只更新marked，再真实unmark；content_serial未变也不能续用旧输入serial。
                let content = doc.cell_draft_facts(&token).unwrap().content_serial;
                edit(
                    &mut doc,
                    &token,
                    0..0,
                    "",
                    MarkdownTextSelection::caret(0),
                    Some(0..1),
                )
                .unwrap();
                let edit = doc.capture_cell_draft_edit(&token).unwrap();
                doc.unmark_cell_draft(edit).unwrap();
                assert_eq!(
                    doc.cell_draft_facts(&token).unwrap().content_serial,
                    content
                );
            }
        }
        let current = doc.cell_draft_facts(&token).unwrap();
        let baseline = doc.storage_profile();
        assert!(matches!(
            doc.commit_prepared_cell_draft_observed(prepared, NoopObserver),
            Err(MarkdownCellDraftCommitError::Draft(
                MarkdownCellDraftError::Stale
            ))
        ));
        assert_eq!(doc.cell_draft_facts(&token).unwrap(), current);
        assert_eq!(doc.storage_profile(), baseline);
        assert_eq!(canonical(&doc), SOURCE);
        assert_no_admission(&observer);
        assert!(doc.cancel_cell_draft(&token));
    }
}

#[test]
fn foreign_document_cannot_consume_or_reprepare_the_original_capsule() {
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(&mut doc, &token, "candidate");
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let before = doc.cell_draft_facts(&token).unwrap();
    let (_other_dir, mut other) = document(SOURCE);
    let other_token = begin(&mut other);
    let original_edit = doc.capture_cell_draft_edit(&token).unwrap();
    assert!(matches!(
        other.prepare_cell_draft_commit(original_edit),
        Err(MarkdownCellDraftError::Stale)
    ));
    let prepared = prepare(&doc, &token).unwrap();
    assert!(matches!(
        other.commit_prepared_cell_draft_observed(prepared, NoopObserver),
        Err(MarkdownCellDraftCommitError::Draft(
            MarkdownCellDraftError::Stale
        ))
    ));
    assert_no_admission(&observer);
    assert_eq!(doc.cell_draft_facts(&token).unwrap(), before);
    assert_eq!(canonical(&doc), SOURCE);
    assert_eq!(canonical(&other), SOURCE);
    assert!(doc.cancel_cell_draft(&token));
    assert!(other.cancel_cell_draft(&other_token));
}

#[test]
fn new_prepare_preserves_marked_failure_and_legacy_commit_keeps_its_existing_behavior() {
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    edit(
        &mut doc,
        &token,
        0..1,
        "ni",
        MarkdownTextSelection::caret(2),
        Some(0..2),
    )
    .unwrap();
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let before = doc.cell_draft_facts(&token).unwrap();
    assert!(matches!(
        prepare(&doc, &token),
        Err(MarkdownCellDraftError::CompositionActive)
    ));
    assert_eq!(doc.cell_draft_facts(&token).unwrap(), before);
    assert_eq!(before.marked_range, Some(0..2));
    assert_no_admission(&observer);
    assert_eq!(observer.snapshot().peak_prepared_commit_bytes, 0);
    // 旧public入口先前允许原owner明确提交marked内容；新协议不静默改变兼容行为。
    doc.commit_cell_draft_observed(&token, NoopObserver)
        .unwrap()
        .unwrap();
    assert_eq!(canonical(&doc), SOURCE.replacen("| a |", "| ni |", 1));
    assert!(observer.is_released());
}

#[test]
fn unchanged_admission_does_not_call_observer_or_publish_selection_and_history() {
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    let before = doc.storage_profile();
    let selection = doc.selection();
    let prepared = prepare(&doc, &token).unwrap();
    assert!(
        doc.commit_prepared_cell_draft_observed(prepared, Reject)
            .unwrap()
            .is_none()
    );
    assert_eq!(doc.storage_profile(), before);
    assert_eq!(doc.selection(), selection);
    assert_eq!(canonical(&doc), SOURCE);
    assert!(!doc.has_cell_draft());
}

#[test]
fn rejected_observer_rolls_back_real_canonical_candidate_and_keeps_local_history() {
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(&mut doc, &token, "first");
    replace(&mut doc, &token, "candidate");
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let before = doc.cell_draft_facts(&token).unwrap();
    let baseline = doc.storage_profile();
    let prepared = prepare(&doc, &token).unwrap();
    assert!(matches!(
        doc.commit_prepared_cell_draft_observed(prepared, Reject),
        Err(MarkdownCellDraftCommitError::Edit(
            MarkdownObservedEditError::Observer("reject admission publish")
        ))
    ));
    assert!(!doc.has_prepared_text_input());
    assert_no_admission(&observer);
    assert_eq!(doc.cell_draft_facts(&token).unwrap(), before);
    assert_eq!(doc.storage_profile(), baseline);
    assert_eq!(canonical(&doc), SOURCE);
    let edit = doc.capture_cell_draft_edit(&token).unwrap();
    doc.restore_cell_draft_history(edit, MarkdownCellDraftHistoryDirection::Undo)
        .unwrap()
        .unwrap();
    let reader = doc.capture_cell_draft(&token).unwrap();
    assert_eq!(read(&reader), "first");
    drop(reader);
    assert!(doc.cancel_cell_draft(&token));
}

#[test]
fn cancellation_retires_the_slot_until_the_actual_admission_string_drops() {
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(&mut doc, &token, "candidate");
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let prepared = prepare(&doc, &token).unwrap();
    assert!(doc.cancel_cell_draft(&token));
    assert!(doc.has_cell_draft());
    assert!(!observer.snapshot().branch_live);
    assert!(observer.snapshot().prepared_commit_live);
    assert!(!observer.is_released());
    assert!(!doc.drain_cancelled_cell_draft());
    assert_eq!(
        doc.replace_utf16_range(0..0, "late", MarkdownTextSelection::caret(4)),
        Err(MarkdownDocumentBufferError::TextInputPrepared)
    );
    drop(prepared);
    assert!(observer.is_released());
    assert_no_admission(&observer);
    assert!(doc.drain_cancelled_cell_draft());
    assert!(!doc.has_cell_draft());
    assert_eq!(canonical(&doc), SOURCE);
}

#[test]
fn prepared_string_does_not_keep_doc_source_alive_but_still_has_a_real_release_claim() {
    let (_dir, mut doc) = document(SOURCE);
    let backing = doc.original_backing_observer().unwrap();
    let token = begin(&mut doc);
    replace(&mut doc, &token, "candidate");
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let prepared = prepare(&doc, &token).unwrap();
    drop(doc);
    assert!(backing.unmap_succeeded());
    assert_eq!(backing.mapped_bytes(), 0);
    assert!(!backing.failed());
    assert!(!observer.snapshot().branch_live);
    assert!(observer.snapshot().prepared_commit_live);
    assert!(!observer.is_released());
    drop(prepared);
    assert!(observer.is_released());
    assert_no_admission(&observer);
}

#[test]
fn cancelled_ticket_cannot_publish_and_its_worker_drop_releases_only_its_own_claim() {
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(&mut doc, &token, "candidate");
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let prepared = prepare(&doc, &token).unwrap();
    assert!(doc.cancel_cell_draft(&token));
    assert!(matches!(
        doc.commit_prepared_cell_draft_observed(prepared, NoopObserver),
        Err(MarkdownCellDraftCommitError::Draft(
            MarkdownCellDraftError::Stale
        ))
    ));
    assert!(observer.is_released());
    assert!(doc.drain_cancelled_cell_draft());
    let token = begin(&mut doc);
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let prepared = prepare(&doc, &token).unwrap();
    // 准入没有Doc/Source强边，跨线程Drop只释放String与纯原子claim，不执行Native或编辑事务。
    std::thread::spawn(move || drop(prepared)).join().unwrap();
    assert_no_admission(&observer);
    assert!(observer.snapshot().branch_live);
    assert_eq!(canonical(&doc), SOURCE);
    assert!(doc.cancel_cell_draft(&token));
}

#[test]
fn exact_utf16_limit_and_maximal_utf8_or_escaping_stay_within_the_single_sanitize_budget() {
    for scalar in ["中", "|"] {
        let (_dir, mut doc) = document(SOURCE);
        let token = begin(&mut doc);
        let raw = scalar.repeat(MARKDOWN_CELL_DRAFT_COMMIT_UTF16_UNITS);
        replace(&mut doc, &token, &raw);
        let observer = doc.cell_draft_resource_observer(&token).unwrap();
        let prepared = prepare(&doc, &token).unwrap();
        let capacity = observer.snapshot().prepared_commit_bytes;
        assert!(
            capacity <= crate::markdown_viewport_presentation::table_window::TABLE_WINDOW_MAX_BYTES
        );
        assert!(capacity >= raw.len());
        let expected = if scalar == "|" {
            "\\|".repeat(65536)
        } else {
            raw
        };
        doc.commit_prepared_cell_draft_observed(prepared, NoopObserver)
            .unwrap()
            .unwrap();
        assert_eq!(
            canonical(&doc),
            SOURCE.replacen("| a |", &format!("| {expected} |"), 1)
        );
        assert!(observer.is_released());
    }
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(
        &mut doc,
        &token,
        &"😀".repeat(MARKDOWN_CELL_DRAFT_COMMIT_UTF16_UNITS / 2),
    );
    edit(
        &mut doc,
        &token,
        65536..65536,
        "x",
        MarkdownTextSelection::caret(65537),
        None,
    )
    .unwrap();
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let prepared = prepare(&doc, &token).unwrap();
    let candidate = prepared.replacement_backing_observer().unwrap();
    assert!(candidate.mapped_bytes() > 65536);
    assert_eq!(observer.snapshot().peak_prepared_commit_bytes, 0);
    assert_eq!(doc.cell_draft_facts(&token).unwrap().utf16_len, 65537);
    doc.commit_prepared_cell_draft_observed(prepared, NoopObserver)
        .unwrap()
        .unwrap();
    assert_eq!(
        canonical(&doc),
        SOURCE.replacen("| a |", &format!("| {}x |", "😀".repeat(32768)), 1)
    );
    doc.undo_text_edit().unwrap();
    assert_eq!(canonical(&doc), SOURCE);
    assert!(observer.is_released());
}

#[test]
fn actual_source_error_precedes_cancelled_or_stale_admission_and_releases_the_string() {
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(&mut doc, &token, "candidate");
    let reader = doc.capture_cell_draft(&token).unwrap();
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let backing = doc.original_backing_observer().unwrap();
    let prepared = prepare(&doc, &token).unwrap();
    let edit = doc.capture_cell_draft_edit(&token).unwrap();
    doc.set_cell_draft_selection(edit, MarkdownTextSelection::caret(0))
        .unwrap();
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
    assert_no_admission(&observer);
    assert_eq!(observer.snapshot().first_source_errno, Some(libc::EIO));
    assert!(!observer.is_released());
    drop(reader);
    assert!(doc.drain_cancelled_cell_draft());
    drop(doc);
    assert!(observer.is_released());
    assert!(backing.unmap_succeeded());
    assert!(backing.failed());
}

#[test]
fn prepare_source_error_precedes_marked_and_busy_without_releasing_another_live_ticket() {
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(&mut doc, &token, "candidate");
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let prepared = prepare(&doc, &token).unwrap();
    let capacity = observer.snapshot().prepared_commit_bytes;
    edit(
        &mut doc,
        &token,
        0..0,
        "",
        MarkdownTextSelection::caret(0),
        Some(0..1),
    )
    .unwrap();
    let edit = doc.capture_cell_draft_edit(&token).unwrap();
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
        doc.prepare_cell_draft_commit(edit),
        Err(MarkdownCellDraftError::Document(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
    assert!(observer.snapshot().prepared_commit_live);
    assert_eq!(observer.snapshot().prepared_commit_bytes, capacity);
    let Some(CellDraftSlot::Active(pending)) = &doc.cell_draft else {
        panic!("Source失败不得移走草稿")
    };
    assert_eq!(pending.marked_range, Some(0..1));
    drop(prepared);
    assert_no_admission(&observer);
    assert!(doc.cancel_cell_draft(&token));
    assert!(observer.is_released());
}

#[test]
fn source_failure_after_observer_finish_rolls_back_the_prepared_admission_publish() {
    struct FailSource;
    impl MarkdownTextChangeObserver for FailSource {
        type Output = ();
        type Error = std::convert::Infallible;
        fn changed(&mut self, _: MarkdownTextChange) -> Result<(), Self::Error> {
            Ok(())
        }
        fn finish(
            self,
            candidate: MarkdownPreparedText<'_>,
            _: MarkdownTextSelection,
        ) -> Result<(), Self::Error> {
            candidate
                .document
                .original_snapshot
                .as_ref()
                .unwrap()
                .fail_next_reclaim_for_test();
            assert_eq!(
                candidate.document.reclaim_original_read_windows_for_test(),
                Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                    libc::EIO
                ))
            );
            Ok(())
        }
    }
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(&mut doc, &token, "candidate");
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let before = doc.cell_draft_facts(&token).unwrap();
    let baseline = doc.storage_profile();
    let root = doc.root.clone();
    let prepared = prepare(&doc, &token).unwrap();
    assert!(matches!(
        doc.commit_prepared_cell_draft_observed(prepared, FailSource),
        Err(MarkdownCellDraftCommitError::Draft(
            MarkdownCellDraftError::Document(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
            )
        ))
    ));
    assert!(!doc.has_prepared_text_input());
    assert!(piece_tree_links_share_root(&root, &doc.root));
    assert_eq!(doc.storage_profile(), baseline);
    let Some(CellDraftSlot::Active(pending)) = &doc.cell_draft else {
        panic!("原草稿必须仍在")
    };
    assert_eq!(pending.facts(), before);
    assert_no_admission(&observer);
    assert!(doc.cancel_cell_draft(&token));
    assert!(observer.is_released());
}

#[test]
/// 清洗状态跨实际4KiB叶，Unicode空白只全局trim；保存流式发布后仍保留一次Undo/Redo。
fn long_sanitize_boundaries_save_and_history_preserve_exact_bytes() {
    let (directory, original) = document(SOURCE);
    drop(original);
    let workspace = crate::MarkdownWorkspace::new(
        directory.path().to_path_buf(),
        directory.path().join("app-data"),
    );
    let access = workspace.open_session().unwrap();
    // 保存必须由原vault规范路径打开；/var与/private别名不能替代同一关联的真实保存权限。
    let mut doc = access.open_document_buffer("cell-draft.md").unwrap();
    let token = begin(&mut doc);
    let prefix = "\u{a0}\u{2003}";
    let mut raw = format!("{prefix}{}", "a".repeat(4095 - prefix.len()));
    raw.push_str("\\|中 "); // 反斜线位于原byte4095，pipe在下一真实叶，保持已转义形式。
    raw.push_str(&"b".repeat(8190 - raw.len()));
    raw.push_str("\\\\|"); // 偶数反斜线之后的pipe仍须再补一条转义反斜线。
    raw.push_str(&"c".repeat(12_287 - raw.len()));
    raw.push_str("\r\n"); // CR位于叶末，下一叶LF不能多产生空格。
    raw.push_str(&"正文😀".repeat(20_000));
    raw.push_str(&"\u{2003}\t\r\n".repeat(4096));
    replace(&mut doc, &token, &raw);
    let mut expected = format!(
        "{}\\|中 {}\\\\\\|{} ",
        "a".repeat(4095 - prefix.len()),
        "b".repeat(8190 - (4095 + "\\|中 ".len())),
        "c".repeat(12_287 - (8190 + "\\\\|".len()))
    );
    expected.push_str(&"正文😀".repeat(20_000));
    let prepared = prepare(&doc, &token).unwrap();
    let mapping = prepared.replacement_backing_observer().unwrap();
    doc.commit_prepared_cell_draft_observed(prepared, NoopObserver)
        .unwrap()
        .unwrap();
    let full = SOURCE.replacen("| a |", &format!("| {expected} |"), 1);
    assert_eq!(canonical(&doc), full);
    assert_eq!(doc.storage_profile().allocated_edit_segment_bytes, 0);
    access.save_document_buffer(&mut doc).unwrap();
    assert_eq!(std::fs::read(doc.path()).unwrap(), full.as_bytes());
    assert!(!doc.is_dirty());
    assert_eq!(doc.storage_profile().undo_entries, 1);
    doc.undo_text_edit().unwrap();
    assert_eq!(canonical(&doc), SOURCE);
    assert!(doc.is_dirty());
    doc.redo_text_edit().unwrap();
    assert_eq!(canonical(&doc), full);
    assert!(!doc.is_dirty());
    assert!(mapping.peak_managed_resident_bytes() <= 64 * 1024);
    drop(doc);
    assert!(mapping.unmap_succeeded());
}

#[test]
/// 未发布候选的真实EIO拒绝本次事务，但不能把它误记成原raw Source故障。
fn candidate_reclaim_failure_rolls_back_without_poisoning_the_original_draft() {
    struct FailCandidate;
    impl MarkdownTextChangeObserver for FailCandidate {
        type Output = ();
        type Error = std::convert::Infallible;
        fn changed(&mut self, _: MarkdownTextChange) -> Result<(), Self::Error> {
            Ok(())
        }
        fn finish(
            self,
            candidate: MarkdownPreparedText<'_>,
            _: MarkdownTextSelection,
        ) -> Result<(), Self::Error> {
            let MarkdownEditSegment::Frozen(backing) =
                candidate.document.edits.segments.last().unwrap().as_ref()
            else {
                panic!("长候选必须是实际封存段");
            };
            backing.fail_next_reclaim_for_test();
            assert_eq!(
                backing.reclaim_unpinned_for_test(),
                Err(crate::original_backing::OriginalBackingError::Unavailable(
                    libc::EIO
                ))
            );
            Ok(())
        }
    }
    let (_directory, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(&mut doc, &token, &"中😀".repeat(40_000));
    let facts = doc.cell_draft_facts(&token).unwrap();
    let profile = doc.storage_profile();
    let resources = doc.cell_draft_resource_observer(&token).unwrap();
    let prepared = prepare(&doc, &token).unwrap();
    let mapping = prepared.replacement_backing_observer().unwrap();
    assert!(matches!(
        doc.commit_prepared_cell_draft_observed(prepared, FailCandidate),
        Err(MarkdownCellDraftCommitError::Edit(
            MarkdownObservedEditError::Document(
                MarkdownDocumentBufferError::EditStorageUnavailable(libc::EIO)
            )
        ))
    ));
    assert!(mapping.unmap_succeeded());
    assert!(mapping.failed());
    assert_eq!(
        resources.snapshot().first_candidate_cleanup_errno,
        Some(libc::EIO)
    );
    assert_eq!(doc.cell_draft_facts(&token).unwrap(), facts);
    assert_eq!(doc.storage_profile(), profile);
    assert_eq!(canonical(&doc), SOURCE);
    assert_eq!(resources.snapshot().first_source_errno, None);
    let read = doc.capture_cell_draft(&token).unwrap();
    let mut tail = [0; 3];
    read.copy_utf16_code_units(&read.authority(), facts.utf16_len - 3, &mut tail)
        .unwrap();
    assert_eq!(tail, [0x4e2d, 0xd83d, 0xde00]);
    drop(read);
    replace(&mut doc, &token, "继续编辑");
    doc.commit_cell_draft_observed(&token, NoopObserver)
        .unwrap()
        .unwrap();
    assert_eq!(canonical(&doc), SOURCE.replacen("| a |", "| 继续编辑 |", 1));
    assert!(!resources.is_released()); // 后继成功不能把先前真实候选I/O失败洗成健康释放。
}

#[test]
/// 取消和Doc退场不销毁仍由准入票拥有的独立候选；最后票Drop才真正munmap。
fn cancelled_long_ticket_retains_only_its_candidate_until_actual_drop() {
    let (_directory, mut doc) = document(SOURCE);
    let original = doc.original_backing_observer().unwrap();
    let token = begin(&mut doc);
    replace(&mut doc, &token, &"x".repeat(256 * 1024));
    let prepared = prepare(&doc, &token).unwrap();
    let mapping = prepared.replacement_backing_observer().unwrap();
    assert!(doc.cancel_cell_draft(&token));
    drop(doc);
    assert!(original.unmap_succeeded());
    assert_eq!(mapping.mapped_bytes(), 256 * 1024);
    drop(prepared);
    assert!(mapping.unmap_succeeded());
    assert_eq!(mapping.mapped_bytes(), 0);
}

#[test]
/// 全部输入被trim时不建立零长映射；普通关闭无空Undo，追加行仍由原独立测试覆盖。
fn long_whitespace_sanitizes_to_the_existing_empty_cell_semantics() {
    let (_directory, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(&mut doc, &token, &"\u{2003}\r\n\t".repeat(40_000));
    let prepared = prepare(&doc, &token).unwrap();
    assert!(prepared.replacement_backing_observer().is_none());
    doc.commit_prepared_cell_draft_observed(prepared, NoopObserver)
        .unwrap()
        .unwrap();
    assert_eq!(canonical(&doc), SOURCE.replacen("| a |", "|  |", 1));
    doc.undo_text_edit().unwrap();
    assert_eq!(canonical(&doc), SOURCE);
}

#[test]
/// 只在独立测试进程改变临时目录或文件大小限制；真实ENOENT/EFBIG都保原草稿可见可改。
fn anonymous_output_io_failures_preserve_the_original_draft() {
    const CHILD: &str = "OMNI_CELL_OUTPUT_IO_CHILD";
    if let Some(kind) = std::env::var_os(CHILD) {
        let source =
            std::path::PathBuf::from(std::env::var_os("OMNI_CELL_OUTPUT_FIXTURE").unwrap());
        let scratch = std::path::PathBuf::from(std::env::var_os("TMPDIR").unwrap());
        let mut doc = MarkdownDocumentBuffer::open(&source, 32 * 1024 * 1024).unwrap();
        let token = begin(&mut doc);
        replace(&mut doc, &token, &"x".repeat(256 * 1024));
        let resources = doc.cell_draft_resource_observer(&token).unwrap();
        let first = doc.capture_cell_draft(&token).unwrap();
        let second = doc.capture_cell_draft(&token).unwrap();
        let before = doc.cell_draft_facts(&token).unwrap();
        let baseline = doc.storage_profile();
        let result = if kind == "create" {
            std::fs::remove_dir(&scratch).unwrap(); // 原快照与raw文件均已匿名，删除空scratch不影响原字节。
            prepare(&doc, &token)
        } else {
            // SAFETY: rlimit只有整数值；独立子进程暂改自身文件大小上限，不触及并行测试或用户进程。
            let mut old: libc::rlimit = unsafe { std::mem::zeroed() };
            // SAFETY: old指向完整rlimit，getrlimit只写该值，不取得指针所有权。
            assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_FSIZE, &mut old) }, 0);
            let limited = libc::rlimit {
                rlim_cur: 4096,
                rlim_max: old.rlim_max,
            };
            // SAFETY: SIG_IGN是标准handler常量；只在本子进程让真实write返回EFBIG，结束后恢复。
            let signal = unsafe { libc::signal(libc::SIGXFSZ, libc::SIG_IGN) };
            // SAFETY: limited是完整只读rlimit，调用只修改本子进程资源上限。
            assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_FSIZE, &limited) }, 0);
            let result = prepare(&doc, &token);
            // SAFETY: old为本进程先前读得的值，恢复软上限不提升原硬上限。
            assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_FSIZE, &old) }, 0);
            // SAFETY: 恢复刚才返回的同一有效handler，不安装新回调或跨进程信号状态。
            unsafe { libc::signal(libc::SIGXFSZ, signal) };
            result
        };
        let expected = if kind == "create" {
            libc::ENOENT
        } else {
            libc::EFBIG
        };
        assert!(
            matches!(result, Err(MarkdownCellDraftError::Document(MarkdownDocumentBufferError::EditStorageUnavailable(errno))) if errno == expected)
        );
        assert_eq!(doc.cell_draft_facts(&token).unwrap(), before);
        assert_eq!(doc.storage_profile(), baseline);
        assert_eq!(resources.snapshot().first_source_errno, None);
        assert!(!resources.snapshot().prepared_commit_live);
        for reader in [&first, &second] {
            let mut tail = [0; 4];
            reader
                .copy_utf16_code_units(&reader.authority(), before.utf16_len - 4, &mut tail)
                .unwrap();
            assert_eq!(tail, [u16::from(b'x'); 4]);
        }
        drop((first, second));
        replace(&mut doc, &token, "仍可输入");
        assert_eq!(doc.cell_draft_facts(&token).unwrap().utf16_len, 4);
        assert_eq!(canonical(&doc), SOURCE);
        assert!(doc.cancel_cell_draft(&token));
        assert!(resources.is_released());
        return;
    }
    for kind in ["create", "write"] {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("cell.md");
        std::fs::write(&source, SOURCE).unwrap();
        let scratch = directory.path().join("scratch");
        std::fs::create_dir(&scratch).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "markdown_document_buffer::cell_draft::tests::commit_admission::anonymous_output_io_failures_preserve_the_original_draft", "--nocapture"])
            .env(CHILD, kind).env("OMNI_CELL_OUTPUT_FIXTURE", &source).env("TMPDIR", &scratch)
            .output().unwrap();
        assert!(
            output.status.success(),
            "真实匿名输出{kind}失败验证未通过：{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
