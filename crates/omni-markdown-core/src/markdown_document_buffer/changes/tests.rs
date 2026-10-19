//! 合成文件验证真实变化流与事务原子性；测试中的有限正文/Vec不进入生产状态。

use super::*;
use std::cell::Cell;
use std::rc::Rc;
use tempfile::{TempDir, tempdir};

fn fixture(text: &str) -> (TempDir, MarkdownDocumentBuffer) {
    let directory = tempdir().unwrap();
    let path = directory.path().join("synthetic.md");
    std::fs::write(&path, text).unwrap();
    (
        directory,
        MarkdownDocumentBuffer::open(&path, 8 * 1024 * 1024).unwrap(),
    )
}

fn text(document: &MarkdownDocumentBuffer) -> String {
    let mut bytes = Vec::new();
    document.write_to(&mut bytes).unwrap();
    String::from_utf8(bytes).unwrap()
}

#[derive(Debug, Eq, PartialEq)]
struct State {
    body: String,
    root: Option<usize>,
    selection: MarkdownTextSelection,
    generation: u64,
    // 共享旧段会迫使真实append走COW，不能靠深拷贝测试值掩盖写穿。
    append: Vec<Arc<MarkdownEditSegment>>,
    capacity: usize,
    used: usize,
    undo: usize,
    redo: usize,
}

fn state(document: &MarkdownDocumentBuffer) -> State {
    State {
        body: text(document),
        root: document.root.as_ref().map(|root| Arc::as_ptr(root).addr()),
        selection: document.selection,
        generation: document.generation,
        append: document.edits.segments.clone(),
        capacity: document.edits.segments.capacity(),
        used: document.edits.used_bytes,
        undo: document.undo.len(),
        redo: document.redo.len(),
    }
}

#[derive(Default)]
struct Collect(Vec<MarkdownTextChange>);
impl MarkdownTextChangeObserver for Collect {
    type Output = Vec<MarkdownTextChange>;
    type Error = &'static str;
    fn changed(&mut self, change: MarkdownTextChange) -> Result<(), Self::Error> {
        self.0.push(change);
        Ok(())
    }
    fn finish(
        self,
        candidate: MarkdownPreparedText<'_>,
        selection: MarkdownTextSelection,
    ) -> Result<Self::Output, Self::Error> {
        assert!(
            selection.anchor_utf16 <= candidate.utf16_len()
                && selection.head_utf16 <= candidate.utf16_len()
        );
        Ok(self.0)
    }
}

fn change(range: Range<usize>, inserted: usize) -> MarkdownTextChange {
    MarkdownTextChange {
        old_utf16_range: range,
        inserted_utf16_len: inserted,
    }
}

struct Reject {
    at: Option<usize>,
    count: usize,
}
impl MarkdownTextChangeObserver for Reject {
    type Output = ();
    type Error = &'static str;
    fn changed(&mut self, _: MarkdownTextChange) -> Result<(), Self::Error> {
        self.count += 1;
        if self.at == Some(self.count) {
            Err("拒绝变化")
        } else {
            Ok(())
        }
    }
    fn finish(
        self,
        _: MarkdownPreparedText<'_>,
        _: MarkdownTextSelection,
    ) -> Result<(), Self::Error> {
        Err("拒绝候选")
    }
}

fn replacements(
    count: usize,
) -> impl Iterator<
    Item = Result<
        MarkdownReplacement<
            impl Iterator<Item = Result<MarkdownReplacementPart<'static>, MarkdownDocumentBufferError>>,
        >,
        MarkdownDocumentBufferError,
    >,
> {
    (0..count).map(|at| {
        Ok(MarkdownReplacement {
            range_utf16: at..at + 1,
            parts: std::iter::once(Ok(MarkdownReplacementPart::Literal("双".into()))),
        })
    })
}

#[test]
fn single_edit_and_surround_emit_true_forward_and_inverse_boundaries() {
    let (_directory, mut document) = fixture("中😀末");
    let changes = document
        .replace_utf16_range_with_selection_observed(
            1..3,
            "ab",
            MarkdownTextSelection::caret(3),
            MarkdownTextSelection::caret(3),
            Collect::default(),
        )
        .unwrap();
    assert_eq!(changes, [change(1..3, 2)]);
    let (_, inverse) = document
        .undo_text_edit_observed(Collect::default())
        .unwrap()
        .unwrap();
    assert_eq!(inverse, [change(1..3, 2)]);
    assert_eq!(text(&document), "中😀末");
    let (_, changes) = document
        .apply_format_observed(
            MarkdownFormatAction::Bold,
            MarkdownTextSelection {
                anchor_utf16: 4,
                head_utf16: 0,
            },
            Collect::default(),
        )
        .unwrap();
    assert_eq!(changes, [change(0..0, 2), change(4..4, 2)]);
    assert_eq!(text(&document), "**中😀末**");
    let (_, inverse) = document
        .undo_text_edit_observed(Collect::default())
        .unwrap()
        .unwrap();
    assert_eq!(inverse, [change(0..2, 0), change(6..8, 0)]);
    let (_, forward) = document
        .redo_text_edit_observed(Collect::default())
        .unwrap()
        .unwrap();
    assert_eq!(forward, changes);
}

#[test]
fn separated_replacements_preserve_middle_and_history_stream() {
    let (_directory, mut document) = fixture("abcdefghij");
    let items = [0..1, 8..10].into_iter().map(|range| {
        Ok(MarkdownReplacement {
            range_utf16: range,
            parts: std::iter::once(Ok(MarkdownReplacementPart::Literal("双".into()))),
        })
    });
    let (_, changes) = document
        .apply_replacements_observed(
            items,
            document.selection,
            MarkdownTextSelection::caret(0),
            Collect::default(),
        )
        .unwrap();
    assert_eq!(changes, [change(0..1, 1), change(8..10, 1)]);
    assert_eq!(text(&document), "双bcdefgh双");
    let (_, inverse) = document
        .undo_text_edit_observed(Collect::default())
        .unwrap()
        .unwrap();
    assert_eq!(inverse, [change(0..1, 1), change(8..9, 2)]);
    assert_eq!(text(&document), "abcdefghij");
}

#[test]
fn indentation_and_ordered_enter_emit_source_ordered_changes() {
    let (_directory, mut document) = fixture("  a\n  b\n  c");
    let (_, changes) = document
        .apply_editing_command_observed(
            MarkdownEditingCommand::IndentLess,
            MarkdownTextSelection {
                anchor_utf16: 11,
                head_utf16: 0,
            },
            Collect::default(),
        )
        .unwrap();
    assert_eq!(
        changes,
        [change(0..2, 0), change(4..6, 0), change(8..10, 0)]
    );
    assert_eq!(text(&document), "a\nb\nc");
    let (_, inverse) = document
        .undo_text_edit_observed(Collect::default())
        .unwrap()
        .unwrap();
    assert_eq!(inverse, [change(0..0, 2), change(2..2, 2), change(4..4, 2)]);
    let (_directory, mut document) = fixture("1. a\n2. b\n3. c");
    let (_, changes) = document
        .apply_editing_command_observed(
            MarkdownEditingCommand::InsertNewline,
            MarkdownTextSelection::caret(4),
            Collect::default(),
        )
        .unwrap();
    assert_eq!(
        changes,
        [change(4..4, 4), change(5..6, 1), change(10..11, 1)]
    );
    assert_eq!(text(&document), "1. a\n2. \n3. b\n4. c");
}

#[test]
fn dense_prefix_tracks_utf16_and_mixed_line_endings_without_an_edit_list() {
    let mut source = String::new();
    for index in 0..4100 {
        source.push_str(if index % 3 == 0 {
            "😀\r\n"
        } else if index % 3 == 1 {
            "中\r"
        } else {
            "x\n"
        });
    }
    let (_directory, mut document) = fixture(&source);
    let end = document.utf16_len();
    let (_, changes) = document
        .apply_format_observed(
            MarkdownFormatAction::Blockquote,
            MarkdownTextSelection {
                anchor_utf16: 0,
                head_utf16: end,
            },
            Collect::default(),
        )
        .unwrap();
    assert_eq!(changes.len(), 4101);
    let mut at = 0;
    for (index, actual) in changes.iter().enumerate() {
        assert_eq!(*actual, change(at..at, 2));
        if index < 4100 {
            at += if index % 3 == 0 { 4 } else { 2 };
        }
    }
    document
        .undo_text_edit_observed(Collect::default())
        .unwrap();
    assert_eq!(text(&document), source);
}

#[test]
fn changed_or_finish_rejection_restores_append_root_selection_redo_and_generation() {
    let (_directory, mut document) = fixture("abcdefghij");
    document
        .replace_utf16_range(0..1, "Z", MarkdownTextSelection::caret(1))
        .unwrap();
    document.undo_text_edit().unwrap();
    for at in [Some(3), None] {
        let before = state(&document);
        let result = document.apply_replacements_observed(
            replacements(5),
            document.selection,
            MarkdownTextSelection::caret(0),
            Reject { at, count: 0 },
        );
        assert!(matches!(
            result,
            Err(MarkdownObservedEditError::Observer(_))
        ));
        assert_eq!(state(&document), before);
    }
    assert_eq!(text(&document), "abcdefghij");
    document.redo_text_edit().unwrap();
    assert_eq!(text(&document), "Zbcdefghij");
}

#[test]
fn journal_write_failure_prevents_commit_and_read_failure_prevents_history_pop() {
    let (_directory, mut document) = fixture("abcdefghij");
    let before = state(&document);
    document
        .change_journal
        .fail_next(change_journal::Fault::PartialWrite);
    assert!(matches!(
        document.apply_replacements_observed(
            replacements(5),
            document.selection,
            MarkdownTextSelection::caret(0),
            Collect::default()
        ),
        Err(MarkdownObservedEditError::Journal(
            MarkdownChangeJournalError::WriteUnavailable(_)
        ))
    ));
    assert_eq!(state(&document), before);
    document
        .apply_replacements(
            replacements(5),
            document.selection,
            MarkdownTextSelection::caret(0),
        )
        .unwrap();
    let before = state(&document);
    document
        .change_journal
        .fail_next(change_journal::Fault::Read);
    assert!(matches!(
        document.undo_text_edit_observed(Collect::default()),
        Err(MarkdownObservedEditError::Journal(
            MarkdownChangeJournalError::ReadUnavailable(_)
        ))
    ));
    assert_eq!(state(&document), before);
    document
        .undo_text_edit_observed(Collect::default())
        .unwrap();
    assert_eq!(text(&document), "abcdefghij");
}

#[test]
fn history_observer_rejection_never_pops_either_direction() {
    let (_directory, mut document) = fixture("abc");
    document
        .replace_utf16_range(1..2, "双双", MarkdownTextSelection::caret(1))
        .unwrap();
    let before = state(&document);
    assert!(
        document
            .undo_text_edit_observed(Reject { at: None, count: 0 })
            .is_err()
    );
    assert_eq!(state(&document), before);
    document.undo_text_edit().unwrap();
    let before = state(&document);
    assert!(
        document
            .redo_text_edit_observed(Reject {
                at: Some(1),
                count: 0
            })
            .is_err()
    );
    assert_eq!(state(&document), before);
}

#[test]
fn no_text_change_does_not_create_a_false_delete_but_can_reject_explicit_head() {
    let (_directory, mut document) = fixture("abc");
    let before = state(&document);
    assert!(
        document
            .replace_utf16_range_with_selection_observed(
                1..1,
                "",
                document.selection,
                MarkdownTextSelection::caret(1),
                Reject {
                    at: Some(1),
                    count: 0
                }
            )
            .is_err()
    );
    assert_eq!(state(&document), before);
    let changes = document
        .replace_utf16_range_with_selection_observed(
            1..1,
            "",
            document.selection,
            MarkdownTextSelection::caret(1),
            Collect::default(),
        )
        .unwrap();
    assert!(changes.is_empty());
    assert_eq!(document.generation, 0);
    assert_eq!(document.selection, MarkdownTextSelection::caret(1));
}

struct Count {
    count: usize,
    calls: Rc<Cell<usize>>,
}
impl MarkdownTextChangeObserver for Count {
    type Output = usize;
    type Error = &'static str;
    fn changed(&mut self, _: MarkdownTextChange) -> Result<(), Self::Error> {
        self.count += 1;
        self.calls.set(self.count);
        Ok(())
    }
    fn finish(
        self,
        _: MarkdownPreparedText<'_>,
        _: MarkdownTextSelection,
    ) -> Result<usize, Self::Error> {
        Ok(self.count)
    }
}

#[test]
fn many_true_changes_stream_once_and_are_replayed_from_paged_history() {
    let (_directory, mut document) = fixture(&"a".repeat(100_000));
    let calls = Rc::new(Cell::new(0));
    let (_, count) = document
        .apply_replacements_observed(
            replacements(100_000),
            document.selection,
            MarkdownTextSelection::caret(0),
            Count {
                count: 0,
                calls: Rc::clone(&calls),
            },
        )
        .unwrap();
    assert_eq!(count, 100_000);
    assert_eq!(calls.get(), count);
    let (_, inverse_count) = document
        .undo_text_edit_observed(Count { count: 0, calls })
        .unwrap()
        .unwrap();
    assert_eq!(inverse_count, count);
}

struct FoldObserver(crate::markdown_folding::anchor_mapping::MarkdownFoldAnchorMapping);
impl MarkdownTextChangeObserver for FoldObserver {
    type Output = Vec<crate::markdown_folding::MarkdownFoldRegion>;
    type Error = &'static str;
    fn changed(&mut self, change: MarkdownTextChange) -> Result<(), Self::Error> {
        self.0
            .push(
                crate::markdown_folding::anchor_mapping::MarkdownFoldChange {
                    old_utf16_range: change.old_utf16_range,
                    inserted_utf16_len: change.inserted_utf16_len,
                },
            )
            .map_err(|_| "折叠映射失败")
    }
    fn finish(
        self,
        candidate: MarkdownPreparedText<'_>,
        _: MarkdownTextSelection,
    ) -> Result<Self::Output, Self::Error> {
        self.0
            .finish(candidate.utf16_len(), None)
            .map_err(|_| "折叠长度失败")?
            .into_iter()
            .map(|range| {
                Ok(crate::markdown_folding::MarkdownFoldRegion {
                    header_line: candidate
                        .line_for_utf16(range.start)
                        .map_err(|_| "候选行号失败")?,
                    hidden_utf16_range: range,
                })
            })
            .collect()
    }
}

#[test]
fn fold_candidate_uses_true_changes_and_unpublished_text_for_new_line_numbers() {
    use crate::markdown_folding::{MarkdownFoldRegion, anchor_mapping::MarkdownFoldAnchorMapping};
    let (_directory, mut document) = fixture("a\nbbb\nccc\nz");
    let folds = [MarkdownFoldRegion {
        header_line: 1,
        hidden_utf16_range: 2..9,
    }];
    let observer =
        FoldObserver(MarkdownFoldAnchorMapping::new(document.utf16_len(), &folds, true).unwrap());
    let items = [(0..1, "aa\n"), (10..11, "end")]
        .into_iter()
        .map(|(range, text)| {
            Ok(MarkdownReplacement {
                range_utf16: range,
                parts: std::iter::once(Ok(MarkdownReplacementPart::Literal(text.into()))),
            })
        });
    let (_, next) = document
        .apply_replacements_observed(
            items,
            document.selection,
            MarkdownTextSelection::caret(0),
            observer,
        )
        .unwrap();
    assert_eq!(
        next,
        [MarkdownFoldRegion {
            header_line: 2,
            hidden_utf16_range: 4..11
        }]
    );
    // Undo从journal真实逆流映射，不把整个首尾通知包络当删除中间fold。
    let observer =
        FoldObserver(MarkdownFoldAnchorMapping::new(document.utf16_len(), &next, true).unwrap());
    let (_, restored) = document.undo_text_edit_observed(observer).unwrap().unwrap();
    assert_eq!(restored, folds);
}

#[test]
fn explicit_empty_replacement_items_do_not_invent_zero_width_delete_events() {
    let (_directory, mut document) = fixture("abc");
    let items = std::iter::once(Ok(MarkdownReplacement {
        range_utf16: 1..1,
        parts: std::iter::empty::<
            Result<MarkdownReplacementPart<'static>, MarkdownDocumentBufferError>,
        >(),
    }));
    let (_, changes) = document
        .apply_replacements_observed(
            items,
            document.selection,
            MarkdownTextSelection::caret(0),
            Collect::default(),
        )
        .unwrap();
    assert!(changes.is_empty());
    // 兼容旧producer的history策略，真实变化描述不因它创建entry就伪造一项内容变化。
    let (_, inverse) = document
        .undo_text_edit_observed(Collect::default())
        .unwrap()
        .unwrap();
    assert!(inverse.is_empty());
    assert_eq!(text(&document), "abc");
}

#[test]
fn generic_observer_and_cleanup_errors_have_usable_display_without_recursive_bounds() {
    let cause = MarkdownObservedEditError::Observer("折叠候选拒绝");
    assert_eq!(cause.to_string(), "折叠候选拒绝");
    let combined = MarkdownObservedEditError::RollbackUnconfirmed {
        cause: Box::new(cause),
        cleanup: MarkdownChangeJournalError::TruncateUnavailable(MarkdownChangeJournalIoFailure {
            kind: std::io::ErrorKind::PermissionDenied,
            raw_os_error: Some(libc::EACCES),
        }),
    };
    assert_eq!(
        combined.to_string(),
        "折叠候选拒绝；变化日志失败后的截尾清理未确认"
    );
}

#[test]
fn observed_error_source_preserves_observer_first_cause_and_legacy_categories() {
    use std::error::Error;
    #[derive(Debug, thiserror::Error)]
    #[error("观察者首因")]
    struct Failure;
    let error = MarkdownObservedEditError::RollbackUnconfirmed {
        cause: Box::new(MarkdownObservedEditError::Observer(Failure)),
        cleanup: MarkdownChangeJournalError::TruncateUnavailable(MarkdownChangeJournalIoFailure {
            kind: std::io::ErrorKind::Other,
            raw_os_error: None,
        }),
    };
    assert_eq!(error.source().unwrap().to_string(), "观察者首因");
    assert!(error.source().unwrap().source().unwrap().is::<Failure>());
    assert_eq!(
        legacy_error(MarkdownObservedEditError::Journal(
            MarkdownChangeJournalError::BudgetExceeded
        )),
        MarkdownDocumentBufferError::ChangeJournalLimitExceeded
    );
    assert_eq!(
        legacy_error(MarkdownObservedEditError::Journal(
            MarkdownChangeJournalError::Corrupt
        )),
        MarkdownDocumentBufferError::ChangeJournalUnavailable
    );
    assert_eq!(
        legacy_error(MarkdownObservedEditError::RollbackUnconfirmed {
            cause: Box::new(MarkdownObservedEditError::Document(
                MarkdownDocumentBufferError::DocumentTooLarge
            )),
            cleanup: MarkdownChangeJournalError::TruncateUnavailable(
                MarkdownChangeJournalIoFailure {
                    kind: std::io::ErrorKind::Other,
                    raw_os_error: None
                }
            ),
        }),
        MarkdownDocumentBufferError::ChangeJournalCleanupUnconfirmed
    );
}
