//! 真实合成文件覆盖准备不可见、失败回滚、唯一权限及取消后的追加资源边界。

use super::*;
use crate::markdown_document_buffer::changes::{
    MarkdownPreparedText, MarkdownTextChange, NoopObserver,
};
use std::rc::Rc;
use tempfile::{TempDir, tempdir};

fn fixture(source: &str, maximum: u64) -> (TempDir, MarkdownDocumentBuffer) {
    // 每项测试只打开独占临时文件，不读取用户笔记或复用其他测试的保存基线。
    let directory = tempdir().unwrap();
    let path = directory.path().join("prepared-input.md");
    std::fs::write(&path, source).unwrap();
    let document = MarkdownDocumentBuffer::open(&path, maximum).unwrap();
    (directory, document)
}

fn body(document: &MarkdownDocumentBuffer) -> Vec<u8> {
    let mut result = Vec::new();
    document.write_to(&mut result).unwrap();
    result
}

#[derive(Debug, Eq, PartialEq)]
struct Canonical {
    body: Vec<u8>,
    root: Option<usize>,
    generation: u64,
    selection: MarkdownTextSelection,
    dirty: bool,
    revision: MarkdownDocumentRevision,
    undo: Vec<(Option<usize>, MarkdownTextSelection)>,
    redo: Vec<(Option<usize>, MarkdownTextSelection)>,
}

fn canonical(document: &MarkdownDocumentBuffer) -> Canonical {
    let history = |entries: &VecDeque<MarkdownEditorMemento>| {
        entries
            .iter()
            .map(|entry| {
                (
                    entry.root.as_ref().map(|root| Arc::as_ptr(root).addr()),
                    entry.selection,
                )
            })
            .collect()
    };
    Canonical {
        body: body(document),
        root: document.root.as_ref().map(|root| Arc::as_ptr(root).addr()),
        generation: document.generation,
        selection: document.selection,
        dirty: document.is_dirty(),
        revision: document.current_revision().unwrap(),
        undo: history(&document.undo),
        redo: history(&document.redo),
    }
}

#[derive(Debug, Eq, PartialEq)]
struct Append {
    // 测试持有原段引用以覆盖真实COW；值比较仍核对失败前后的全部段字节。
    segments: Vec<Arc<MarkdownEditSegment>>,
    outer_capacity: usize,
    allocated_bytes: usize,
    used: usize,
}

fn append(document: &MarkdownDocumentBuffer) -> Append {
    Append {
        segments: document.edits.segments.clone(),
        outer_capacity: document.edits.segments.capacity(),
        allocated_bytes: document.edits.allocated_bytes(),
        used: document.edits.used_bytes,
    }
}

#[derive(Default)]
struct Observe {
    changes: Vec<MarkdownTextChange>,
    reject_finish: bool,
}

impl MarkdownTextChangeObserver for Observe {
    type Output = (Vec<MarkdownTextChange>, usize, MarkdownTextSelection);
    type Error = &'static str;

    fn changed(&mut self, change: MarkdownTextChange) -> Result<(), Self::Error> {
        // 只收集本项测试的有限变化，不给生产引入正文或全量编辑镜像。
        self.changes.push(change);
        Ok(())
    }

    fn finish(
        self,
        candidate: MarkdownPreparedText<'_>,
        selection: MarkdownTextSelection,
    ) -> Result<Self::Output, Self::Error> {
        if self.reject_finish {
            // 在候选append已产生后拒绝，检验真正的回滚切口而非仅前置范围校验。
            return Err("候选观察者拒绝");
        }
        Ok((self.changes, candidate.utf16_len(), selection))
    }
}

fn assert_busy<T, E>(result: Result<T, MarkdownObservedEditError<E>>) {
    assert!(matches!(
        result,
        Err(MarkdownObservedEditError::Document(
            MarkdownDocumentBufferError::TextInputPrepared
        ))
    ));
}

#[test]
fn prepared_input_is_invisible_then_publishes_once_with_full_history_selection() {
    let (_directory, mut document) = fixture("前😀后\r\n末", 1024);
    let before = MarkdownTextSelection {
        anchor_utf16: 3,
        head_utf16: 1,
    };
    let after = MarkdownTextSelection {
        anchor_utf16: 4,
        head_utf16: 2,
    };
    document.set_selection(before).unwrap();
    let original = canonical(&document);
    let (token, (changes, units, observed_selection)) = document
        .prepare_text_input_observed(1..3, "中😀文", before, after, Observe::default())
        .unwrap();
    assert_eq!(canonical(&document), original);
    assert_eq!(
        changes,
        [MarkdownTextChange {
            old_utf16_range: 1..3,
            inserted_utf16_len: 4
        }]
    );
    assert_eq!(units, document.utf16_len() + 2);
    assert_eq!(observed_selection, after);
    assert!(document.has_prepared_text_input());

    let edit = document.publish_text_input(&token).unwrap();
    assert_eq!(edit.replaced_utf16_range, 1..3);
    assert_eq!(edit.change_in_utf16_length, 2);
    assert_eq!(edit.selection_after, after);
    assert_eq!(body(&document), "前中😀文后\r\n末".as_bytes());
    assert_eq!(document.generation(), original.generation + 1);
    assert!(!document.has_prepared_text_input());
    let published = canonical(&document);
    assert_eq!(
        document.publish_text_input(&token),
        Err(MarkdownDocumentBufferError::PreparedTextInputStale)
    );
    assert_eq!(canonical(&document), published);
    assert_eq!(document.undo_text_edit().unwrap().selection_after, before);
    assert_eq!(body(&document), original.body);
    assert_eq!(document.redo_text_edit().unwrap().selection_after, after);
    assert_eq!(body(&document), published.body);
}

#[test]
/// 两阶段间的共享reader回收失败撤销发布；候选、历史和原nonce取消能力仍完整保留。
fn poisoned_shared_source_rejects_prepared_publication_without_consuming_its_nonce() {
    let (_directory, mut document) = fixture("前😀后", 4096);
    let original = canonical(&document);
    let original_append = append(&document);
    let snapshot = document.text_snapshot();
    let mapping = document.original_backing_observer().unwrap();
    let before = document.selection();
    let (token, _) = document
        .prepare_text_input_observed(
            1..3,
            "中文",
            before,
            MarkdownTextSelection::caret(3),
            Observe::default(),
        )
        .unwrap();
    let prepared_append = append(&document);
    // 已完成真实候选准备后才触发真实msync后的注入错误，不能只测prepare前的显然失败。
    document.fail_next_original_reclaim_for_test();
    assert_eq!(
        document.reclaim_original_read_windows_for_test(),
        Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
            libc::EIO
        ))
    ); // 显式驱逐无pin窗口，故障标志必须经过真实msync后才成为共享永久失败。
    assert_eq!(
        snapshot.visit_byte_range(0..1, |_| Ok(())),
        Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
            libc::EIO
        ))
    );
    assert_eq!(
        document.publish_text_input(&token),
        Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
            libc::EIO
        ))
    );
    assert!(document.has_prepared_text_input());
    assert_eq!(
        document.root.as_ref().map(|root| Arc::as_ptr(root).addr()),
        original.root
    );
    assert_eq!(document.generation(), original.generation);
    assert_eq!(document.selection(), original.selection);
    assert_eq!(document.is_dirty(), original.dirty);
    assert_eq!(document.undo.len(), original.undo.len());
    assert_eq!(document.redo.len(), original.redo.len());
    assert_eq!(append(&document), prepared_append); // 发布拒绝不能提前回滚原owner仍持有的候选。
    assert_eq!(std::fs::read(document.path()).unwrap(), original.body);
    // 原nonce仍可明确取消并回收追加/日志，不需要成功读取已撤权的原正文。
    document.cancel_prepared_text_input(&token).unwrap();
    assert!(!document.has_prepared_text_input());
    assert_eq!(append(&document), original_append);
    drop(snapshot);
    drop(document);
    assert!(mapping.unmap_succeeded());
    assert_eq!(mapping.mapped_bytes(), 0);
    assert!(mapping.failed()); // 最终真实unmap不擦除早先已经发生的源失败。
}

#[test]
fn capacity_unicode_append_and_observer_fail_before_reservation_and_preserve_all_facts() {
    let (_directory, mut document) = fixture("😀ab", 8);
    let selection = MarkdownTextSelection::caret(4);
    document.set_selection(selection).unwrap();
    let original = canonical(&document);
    let original_append = append(&document);
    let result = document.prepare_text_input_observed(
        0..0,
        "12345",
        selection,
        selection,
        Observe::default(),
    );
    assert!(matches!(
        result,
        Err(MarkdownObservedEditError::Document(
            MarkdownDocumentBufferError::DocumentTooLarge
        ))
    ));
    assert_eq!(canonical(&document), original);
    assert_eq!(append(&document), original_append);
    let result =
        document.prepare_text_input_observed(1..2, "x", selection, selection, Observe::default());
    assert!(matches!(
        result,
        Err(MarkdownObservedEditError::Document(
            MarkdownDocumentBufferError::NotUnicodeBoundary
        ))
    ));
    assert_eq!(canonical(&document), original);
    assert_eq!(append(&document), original_append);
    let result = document.prepare_text_input_observed(
        2..3,
        "中",
        selection,
        selection,
        Observe {
            reject_finish: true,
            ..Observe::default()
        },
    );
    assert!(matches!(
        result,
        Err(MarkdownObservedEditError::Observer("候选观察者拒绝"))
    ));
    assert_eq!(canonical(&document), original);
    assert_eq!(append(&document), original_append);
    assert!(!document.has_prepared_text_input());

    // live文档仍小，但已发布的累计append耗尽；不能用结果长度取代会话真实预算。
    let (_directory, mut document) = fixture("a", 4);
    for _ in 0..4 {
        document
            .replace_utf16_range_with_selection(
                0..1,
                "a",
                MarkdownTextSelection::caret(0),
                MarkdownTextSelection::caret(1),
            )
            .unwrap();
    }
    let original = canonical(&document);
    let original_append = append(&document);
    let result = document.prepare_text_input_observed(
        0..1,
        "b",
        document.selection(),
        MarkdownTextSelection::caret(1),
        Observe::default(),
    );
    assert!(matches!(
        result,
        Err(MarkdownObservedEditError::Document(
            MarkdownDocumentBufferError::EditBufferLimitExceeded
        ))
    ));
    assert_eq!(canonical(&document), original);
    assert_eq!(append(&document), original_append);
    assert!(!document.has_prepared_text_input());
}

#[test]
fn reservation_rejects_every_central_mutation_including_empty_commands_and_save_owners() {
    let (_directory, mut document) = fixture("abc", 1024);
    document
        .replace_utf16_range_with_selection(
            0..1,
            "A",
            MarkdownTextSelection::caret(0),
            MarkdownTextSelection::caret(1),
        )
        .unwrap();
    document.undo_text_edit().unwrap();
    let original = canonical(&document);
    let before = document.selection();
    let (token, ()) = document
        .prepare_text_input_observed(
            1..2,
            "中",
            before,
            MarkdownTextSelection::caret(2),
            NoopObserver,
        )
        .unwrap();
    let prepared_append = append(&document);
    assert_eq!(
        document.set_selection(before),
        Err(MarkdownDocumentBufferError::TextInputPrepared)
    );
    assert_busy(document.replace_utf16_range_with_selection_observed(
        0..0,
        "",
        before,
        before,
        NoopObserver,
    ));
    assert_busy(document.apply_replacements_observed(
        std::iter::empty::<
            Result<
                MarkdownReplacement<
                    std::iter::Empty<
                        Result<MarkdownReplacementPart<'static>, MarkdownDocumentBufferError>,
                    >,
                >,
                MarkdownDocumentBufferError,
            >,
        >(),
        before,
        before,
        NoopObserver,
    ));
    assert_busy(document.apply_editing_command_observed(
        MarkdownEditingCommand::DeleteMarkupBackward,
        before,
        NoopObserver,
    ));
    assert_busy(document.apply_format_observed(MarkdownFormatAction::Bold, before, NoopObserver));
    assert_busy(document.undo_text_edit_observed(NoopObserver));
    assert_busy(document.redo_text_edit_observed(NoopObserver));
    let path = document.path().to_path_buf();
    let saved = MarkdownDocumentBuffer::open(&path, 1024).unwrap();
    assert_eq!(
        document.adopt_atomic_save_destination(&path),
        Err(MarkdownDocumentBufferError::TextInputPrepared)
    );
    assert_eq!(
        document.adopt_saved_baseline(&saved),
        Err(MarkdownDocumentBufferError::TextInputPrepared)
    );
    assert_busy(command_transaction::commit_prepared_command_observed(
        &mut document,
        before,
        NoopObserver,
        |_, _| panic!("busy候选不得执行准备闭包"),
    ));
    assert_eq!(canonical(&document), original);
    assert_eq!(append(&document), prepared_append);
    document.cancel_prepared_text_input(&token).unwrap();
    assert_eq!(canonical(&document), original);
}

#[test]
fn wrong_empty_document_and_stale_tokens_cannot_cancel_successor_append() {
    let (_directory_a, mut first) = fixture("", 1024);
    let (_directory_b, mut second) = fixture("", 1024);
    let zero = MarkdownTextSelection::caret(0);
    let (old, ()) = first
        .prepare_text_input_observed(
            0..0,
            "甲",
            zero,
            MarkdownTextSelection::caret(1),
            NoopObserver,
        )
        .unwrap();
    let (other, ()) = second
        .prepare_text_input_observed(
            0..0,
            "乙",
            zero,
            MarkdownTextSelection::caret(1),
            NoopObserver,
        )
        .unwrap();
    let second_append = append(&second);
    assert_eq!(
        second.publish_text_input(&old),
        Err(MarkdownDocumentBufferError::PreparedTextInputStale)
    );
    assert_eq!(
        second.cancel_prepared_text_input(&old),
        Err(MarkdownDocumentBufferError::PreparedTextInputStale)
    );
    assert_eq!(append(&second), second_append);
    first.cancel_prepared_text_input(&old).unwrap();
    let (successor, ()) = first
        .prepare_text_input_observed(
            0..0,
            "新",
            zero,
            MarkdownTextSelection::caret(1),
            NoopObserver,
        )
        .unwrap();
    let successor_append = append(&first);
    assert_eq!(
        first.cancel_prepared_text_input(&old),
        Err(MarkdownDocumentBufferError::PreparedTextInputStale)
    );
    drop(old);
    assert!(!first.drain_cancelled_text_input());
    assert_eq!(append(&first), successor_append);
    first.publish_text_input(&successor).unwrap();
    second.publish_text_input(&other).unwrap();
    assert_eq!(body(&first), "新".as_bytes());
    assert_eq!(body(&second), "乙".as_bytes());
}

#[test]
fn cancellation_releases_candidate_then_restores_append_and_drop_only_marks_pending() {
    let (_directory, mut document) = fixture("原文", 128 * 1024);
    let original = canonical(&document);
    let original_append = append(&document);
    let (token, ()) = document
        .prepare_text_input_observed(
            0..0,
            &"x".repeat(70 * 1024),
            document.selection(),
            MarkdownTextSelection::caret(0),
            NoopObserver,
        )
        .unwrap();
    let pending = document.prepared_text_input.as_ref().unwrap();
    let candidate = match &pending.command {
        PendingCommand::Characters(prepared) => {
            Arc::downgrade(prepared.command.root.as_ref().unwrap())
        }
        PendingCommand::SelectionOnly { .. } => panic!("本项必须产生正文候选"),
    };
    let nonce = token.nonce.clone();
    drop(token);
    assert!(document.has_prepared_text_input());
    assert!(candidate.upgrade().is_some());
    assert_eq!(canonical(&document), original);
    assert_eq!(
        document.set_selection(document.selection()),
        Err(MarkdownDocumentBufferError::TextInputPrepared)
    );
    assert!(document.drain_cancelled_text_input());
    assert!(!document.drain_cancelled_text_input());
    assert!(candidate.upgrade().is_none());
    assert!(nonce.upgrade().is_none());
    assert_eq!(canonical(&document), original);
    assert_eq!(append(&document), original_append);
    // 取消尾部不消耗会话append资格，后继真实输入仍能正常提交并撤销。
    document
        .replace_utf16_range_with_selection(
            0..0,
            "新",
            document.selection(),
            MarkdownTextSelection::caret(1),
        )
        .unwrap();
    document.undo_text_edit().unwrap();
    assert_eq!(body(&document), original.body);
}

#[test]
fn selection_only_preparation_preserves_generation_and_redo_then_publishes_selection() {
    let (_directory, mut document) = fixture("abc", 1024);
    document
        .replace_utf16_range_with_selection(
            0..1,
            "A",
            MarkdownTextSelection::caret(0),
            MarkdownTextSelection::caret(1),
        )
        .unwrap();
    document.undo_text_edit().unwrap();
    let original = canonical(&document);
    let original_append = append(&document);
    let after = MarkdownTextSelection {
        anchor_utf16: 3,
        head_utf16: 1,
    };
    let (token, ()) = document
        .prepare_text_input_observed(1..1, "", document.selection(), after, NoopObserver)
        .unwrap();
    assert_eq!(canonical(&document), original);
    assert_eq!(append(&document), original_append);
    let edit = document.publish_text_input(&token).unwrap();
    assert_eq!(edit.replaced_utf16_range, 1..1);
    assert_eq!(edit.change_in_utf16_length, 0);
    assert_eq!(document.selection(), after);
    assert_eq!(document.generation(), original.generation);
    assert_eq!(document.redo.len(), original.redo.len());
    assert_eq!(document.undo.len(), original.undo.len());
}

#[test]
fn token_outliving_document_keeps_neither_canonical_nor_candidate_source_graph_alive() {
    let (_directory, mut document) = fixture("映射😀正文", 1024);
    let original = Arc::downgrade(document.root.as_ref().unwrap());
    let (token, ()) = document
        .prepare_text_input_observed(
            0..0,
            "新",
            document.selection(),
            MarkdownTextSelection::caret(1),
            NoopObserver,
        )
        .unwrap();
    let candidate = match &document.prepared_text_input.as_ref().unwrap().command {
        PendingCommand::Characters(prepared) => {
            Arc::downgrade(prepared.command.root.as_ref().unwrap())
        }
        PendingCommand::SelectionOnly { .. } => panic!("本项必须产生正文候选"),
    };
    let owner = Rc::new(document);
    let weak_owner = Rc::downgrade(&owner);
    drop(owner);
    assert!(weak_owner.upgrade().is_none());
    assert!(original.upgrade().is_none());
    assert!(candidate.upgrade().is_none());
    assert!(token.nonce.upgrade().is_none());
    // 原映射只存在已释放document字段中；token无路径、文件、root或source持有边。
    drop(token);
}

#[test]
fn shared_prepare_rolls_back_real_paged_journal_failure_before_any_publication() {
    let (_directory, mut document) = fixture("abc", 1024);
    let original = canonical(&document);
    let original_append = append(&document);
    document
        .change_journal
        .fail_next(change_journal::Fault::PartialWrite);
    let result = command_transaction::prepare_command_observed(
        &mut document,
        MarkdownTextSelection::caret(0),
        NoopObserver,
        |document, observer| {
            // 单次键入通常inline；这里走同一准备切口的真实三变化页，注入实际部分pwrite失败。
            for at in 0..3 {
                changes::emit(document, observer, at..at + 1, 1)?;
            }
            let _candidate_bytes = document
                .edits
                .append("候选", document.maximum_document_bytes)?;
            Ok(PreparedCommand {
                root: document.root.clone(),
                after: document.selection(),
                metrics: MarkdownReplacementMetrics::new(0, 3, 3)?,
            })
        },
    );
    assert!(matches!(
        result,
        Err(MarkdownObservedEditError::Journal(
            change_journal::MarkdownChangeJournalError::WriteUnavailable(_)
        ))
    ));
    assert_eq!(canonical(&document), original);
    assert_eq!(append(&document), original_append);
    assert!(!document.has_prepared_text_input());
    // 失败日志页已释放，随后正常事务与Undo仍能使用同一journal所有权。
    document
        .replace_utf16_range_with_selection(
            0..1,
            "甲",
            document.selection(),
            MarkdownTextSelection::caret(1),
        )
        .unwrap();
    assert_eq!(
        document.undo_text_edit().unwrap().selection_after,
        original.selection
    );
    assert_eq!(body(&document), original.body);
}

#[test]
fn pure_document_and_pending_token_preserve_send_capability() {
    // domain仍可移交纯Rust线程；原生主线程要求不通过nonce扩散为Core类型限制。
    fn require_send<T: Send>() {}
    require_send::<MarkdownDocumentBuffer>();
    require_send::<MarkdownPreparedTextInput>();
}

#[test]
fn pending_input_blocks_save_and_save_as_before_any_disk_or_saved_baseline_mutation() {
    // 保存回归使用独立vault；普通保存已有dirty草稿，保证未被“clean快速返回”掩盖。
    let directory = tempdir().unwrap();
    let root = directory.path().join("vault");
    let workspace = crate::MarkdownWorkspace::new(root.clone());
    let access = workspace.open_session().unwrap();
    std::fs::write(root.join("Note.md"), "before").unwrap();
    std::fs::write(root.join("Draft.md"), "旧目标").unwrap();
    let mut document = access.open_document_buffer("Note.md").unwrap();
    document
        .replace_utf16_range_with_selection(
            0..6,
            "已有草稿",
            MarkdownTextSelection::caret(0),
            MarkdownTextSelection::caret(4),
        )
        .unwrap();
    let original = canonical(&document);
    let original_baseline = document.original_revision();
    let (token, ()) = document
        .prepare_text_input_observed(
            0..1,
            "候选",
            document.selection(),
            MarkdownTextSelection::caret(1),
            NoopObserver,
        )
        .unwrap();
    let prepared_append = append(&document);
    assert_eq!(
        access.save_document_buffer(&mut document).err(),
        Some(crate::MarkdownWorkspaceError::StorageUnavailable)
    );
    assert_eq!(
        access
            .save_document_buffer_as_new(&document, "", "Draft")
            .err(),
        Some(crate::MarkdownWorkspaceError::StorageUnavailable)
    );
    assert_eq!(std::fs::read(root.join("Note.md")).unwrap(), b"before");
    assert_eq!(
        std::fs::read(root.join("Draft.md")).unwrap(),
        "旧目标".as_bytes()
    );
    let mut names = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(
        names,
        [
            std::ffi::OsString::from("Draft.md"),
            std::ffi::OsString::from("Note.md")
        ]
    );
    assert_eq!(canonical(&document), original);
    assert_eq!(document.original_revision(), original_baseline);
    assert_eq!(append(&document), prepared_append);
    assert!(document.has_prepared_text_input());

    // 原owner显式取消后普通保存恢复；阻塞只围住真实reservation，而非永久禁用持久化。
    document.cancel_prepared_text_input(&token).unwrap();
    access.save_document_buffer(&mut document).unwrap();
    assert_eq!(std::fs::read(root.join("Note.md")).unwrap(), original.body);
    assert!(!document.is_dirty());
}
