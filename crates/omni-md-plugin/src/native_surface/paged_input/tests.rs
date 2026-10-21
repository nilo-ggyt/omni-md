//! Exercise production transaction adapters with real Core documents, without NSApplication, hidden input storage, or synthetic geometry.

use super::*;
use std::io::Write;

mod cell;
mod composition;
mod folding;
mod formatting;
mod navigation;
mod replacements;
mod selection_ranges;
mod source_commands;

// Own the private file and canonical allocation together. The caller keeps the
// file alive while exercising production mmap/edit/history APIs against it.
fn fixture(
    text: &str,
    maximum: u64,
) -> (tempfile::NamedTempFile, Rc<RefCell<MarkdownDocumentBuffer>>) {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(text.as_bytes()).unwrap();
    let document = MarkdownDocumentBuffer::open(file.path(), maximum).unwrap();
    (file, Rc::new(RefCell::new(document)))
}

fn text(document: &RefCell<MarkdownDocumentBuffer>) -> String {
    let document = document.borrow();
    assert!(document.utf16_len() <= 256); // Only this bounded short fixture materializes an oracle; production exposes no full-text mirror API.
    let mut units = [0; 256];
    let length = document.utf16_len();
    document
        .copy_utf16_code_units(0, &mut units[..length])
        .unwrap();
    String::from_utf16(&units[..length]).unwrap()
}

fn unspecified() -> NSRange {
    // Use the actual AppKit sentinel so replacement precedence is exercised by
    // production code instead of selecting a range in this fixture helper.
    NSRange::new(NSNotFound as usize, 0)
}

#[test]
fn selection_publication_preserves_generation_bytes_history_and_rejects_split_scalar() {
    let (_file, document) = fixture("甲🙂乙", 4096);
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let before = owner.state().unwrap();
    let permit = owner.capture(1, &|epoch| epoch == 1).unwrap();
    let outcome = owner
        .set_selection(
            &permit,
            MarkdownTextSelection {
                anchor_utf16: 1,
                head_utf16: 3,
            },
            &|epoch| epoch == 1,
        )
        .unwrap();
    let commit = committed(outcome);
    assert_eq!(commit.state.generation, before.generation);
    assert_eq!(commit.state.fold_revision, before.fold_revision);
    assert!(commit.state.selection_serial > before.selection_serial);
    assert!(commit.invalidation.selection_changed && !commit.invalidation.characters_changed);
    assert_eq!(text(&document), "甲🙂乙");
    assert!(!document.borrow().can_undo() && !document.borrow().can_redo());
    let stable = owner.state().unwrap();
    let invalid = owner.capture(1, &|epoch| epoch == 1).unwrap();
    assert!(
        owner
            .set_selection(&invalid, MarkdownTextSelection::caret(2), &|epoch| epoch
                == 1)
            .is_err()
    );
    assert_eq!(owner.state().unwrap(), stable);
    assert_eq!(text(&document), "甲🙂乙");
    let stale = owner.capture(1, &|epoch| epoch == 1).unwrap();
    assert!(matches!(
        owner.set_selection(&stale, MarkdownTextSelection::caret(0), &|_| false),
        Err(NativeMarkdownPagedInputError::Revoked)
    ));
    assert_eq!(owner.state().unwrap(), stable);
}

fn committed(outcome: NativeMarkdownPagedInputOutcome) -> NativeMarkdownPagedInputCommit {
    // These callers expect a user-visible change; an unchanged result is not a
    // substitute for checking the actual commit receipt and canonical data.
    match outcome {
        NativeMarkdownPagedInputOutcome::Committed(commit) => commit,
        NativeMarkdownPagedInputOutcome::Unchanged(_) => panic!("真实变化没有返回提交收据"),
    }
}

#[test]
fn empty_ime_commit_keeps_original_complete_input_and_existing_folds() {
    for folded in [false, true] {
        let original = "# A\n正文\n# B\n尾行";
        let (_file, document) = fixture(original, 4096);
        let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
        let current = |_| true;
        if folded {
            let permit = owner.capture(1, &current).unwrap();
            committed(owner.toggle_fold(&permit, 0, &current).unwrap());
        }
        // Submit empty IME-style input through public preparation/publication so unchanged still requires Core health checks.
        let before = owner.state().unwrap();
        let permit = owner.capture(1, &current).unwrap();
        let folds = owner.folded_layout_ranges(&permit, &current).unwrap();
        let receipt = owner
            .insert_text(&permit, "", unspecified(), &current)
            .unwrap();
        assert_eq!(owner.state().unwrap(), before);
        assert_eq!(receipt.state, before);
        assert!(receipt.check_current(&current).is_ok());
        assert_eq!(
            owner.folded_layout_ranges(&permit, &current).unwrap(),
            folds
        );
        assert_eq!(text(&document), original);
        assert!(!document.borrow().can_undo() && !document.borrow().can_redo());
    }
}

#[test]
fn empty_text_deletes_actual_selection_or_marked_range() {
    for marked in [false, true] {
        let (_file, document) = fixture("甲乙丙", 4096);
        let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
        let current = |_| true;
        let permit = owner.capture(1, &current).unwrap();
        if marked {
            owner
                .set_marked_text(
                    &permit,
                    "中",
                    NSRange::new(1, 0),
                    NSRange::new(0, 1),
                    &current,
                )
                .unwrap();
        } else {
            committed(
                owner
                    .set_selection(
                        &permit,
                        MarkdownTextSelection {
                            anchor_utf16: 0,
                            head_utf16: 1,
                        },
                        &current,
                    )
                    .unwrap(),
            );
        }
        // An empty payload is not necessarily a no-op: delete an actual selection/marked range and preserve its history.
        let before = owner.state().unwrap();
        let permit = owner.capture(1, &current).unwrap();
        let receipt = owner
            .insert_text(&permit, "", unspecified(), &current)
            .unwrap();
        assert!(receipt.state.generation > before.generation);
        assert!(receipt.state.marked.is_none());
        assert_eq!(receipt.state.selection, MarkdownTextSelection::caret(0));
        assert_eq!(text(&document), "乙丙");
        assert!(document.borrow().can_undo());
    }
}

#[test]
fn empty_explicit_replacement_preserves_secondary_range_semantics() {
    let (_file, document) = fixture("甲乙丙", 4096);
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let current = |_| true;
    let permit = owner.capture(1, &current).unwrap();
    committed(
        owner
            .set_selected_ranges(&permit, &[0..0, 2..3], &current)
            .unwrap(),
    );
    let before = owner.state().unwrap();
    let permit = owner.capture(1, &current).unwrap();
    // An unspecified range remains ambiguous; explicit secondary deletion must not disappear behind an unchanged primary caret.
    assert!(matches!(
        owner.insert_text(&permit, "", unspecified(), &current),
        Err(NativeMarkdownPagedInputError::MultipleSelectionInput)
    ));
    assert_eq!(owner.state().unwrap(), before);
    // The explicit range is a new input request, not reuse of the one-shot permit consumed by ambiguity rejection.
    let permit = owner.capture(1, &current).unwrap();
    let receipt = owner
        .insert_text(&permit, "", NSRange::new(2, 1), &current)
        .unwrap();
    assert!(receipt.state.generation > before.generation);
    assert_eq!(receipt.state.selection_range_count, 1);
    assert_eq!(receipt.state.selection, MarkdownTextSelection::caret(2));
    assert_eq!(text(&document), "甲乙");
    assert!(document.borrow().can_undo());
}

#[test]
fn chinese_emoji_marked_replacement_and_history_use_real_core_transactions() {
    let (_file, document) = fixture("首尾", 4096);
    document
        .borrow_mut()
        .set_selection(MarkdownTextSelection::caret(1))
        .unwrap();
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let epoch = Cell::new(1);
    let current = |expected| expected == epoch.get();
    let permit = owner.capture(1, &current).unwrap();
    let first = owner
        .set_marked_text(
            &permit,
            "中🙂文",
            NSRange::new(1, 2),
            unspecified(),
            &current,
        )
        .unwrap();
    assert_eq!(text(&document), "首中🙂文尾");
    assert_eq!(first.state.marked, Some(NSRange::new(1, 4)));
    assert_eq!(
        first.state.selection,
        MarkdownTextSelection {
            anchor_utf16: 2,
            head_utf16: 4
        }
    );
    first.check_current(&current).unwrap();

    // A new foreground epoch does not change composition ownership. Update the
    // original marked range, then confirm it through a new one-shot callback.
    epoch.set(2);
    let permit = owner.capture(2, &current).unwrap();
    let second = owner
        .set_marked_text(&permit, "中文", NSRange::new(2, 0), unspecified(), &current)
        .unwrap();
    assert_eq!(text(&document), "首中文尾");
    assert_eq!(second.state.marked, Some(NSRange::new(1, 2)));
    assert_eq!(second.state.selection, MarkdownTextSelection::caret(3));
    assert!(matches!(
        first.check_current(&current),
        Err(NativeMarkdownPagedInputError::Revoked)
    ));

    epoch.set(3);
    let permit = owner.capture(3, &current).unwrap();
    let inserted = owner
        .insert_text(&permit, "中文🙂", unspecified(), &current)
        .unwrap();
    assert_eq!(text(&document), "首中文🙂尾");
    assert_eq!(inserted.state.marked, None);
    assert_eq!(inserted.state.selection, MarkdownTextSelection::caret(5));
    assert_eq!(inserted.invalidation.replaced_utf16_range, Some(1..3));
    assert_eq!(inserted.invalidation.change_in_utf16_length, 2);
    assert!(inserted.invalidation.characters_changed);
    assert!(matches!(
        owner.insert_text(&permit, "不重复", unspecified(), &current),
        Err(NativeMarkdownPagedInputError::Consumed)
    ));

    // Undo the whole composition once, including its initial caret. Redo must
    // restore actual final text/selection without reviving an IME session.
    epoch.set(4);
    let permit = owner.capture(4, &current).unwrap();
    let undo = committed(
        owner
            .history(&permit, NativeMarkdownPagedInputHistory::Undo, &current)
            .unwrap(),
    );
    assert_eq!(text(&document), "首尾");
    assert_eq!(undo.state.selection, MarkdownTextSelection::caret(1));
    assert_eq!(undo.state.marked, None); // History restores text/selection without inventing a restarted IME session.
    let permit = owner.capture(4, &current).unwrap();
    assert!(matches!(
        owner
            .history(&permit, NativeMarkdownPagedInputHistory::Undo, &current)
            .unwrap(),
        NativeMarkdownPagedInputOutcome::Unchanged(_)
    )); // Candidate updates and confirmation of one composition occupy one history entry.
    epoch.set(5);
    let permit = owner.capture(5, &current).unwrap();
    let redo = committed(
        owner
            .history(&permit, NativeMarkdownPagedInputHistory::Redo, &current)
            .unwrap(),
    );
    assert_eq!(text(&document), "首中文🙂尾");
    assert_eq!(redo.state.selection, inserted.state.selection);
    redo.check_current(&current).unwrap();
}

#[test]
fn unmark_preserves_source_generation_lengths_history_and_actual_mapping() {
    let (_file, document) = fixture("首", 4096);
    document
        .borrow_mut()
        .set_selection(MarkdownTextSelection::caret(1))
        .unwrap();
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let current = |epoch| epoch == 7;
    let permit = owner.capture(7, &current).unwrap();
    owner
        .set_marked_text(
            &permit,
            "中文🙂",
            NSRange::new(4, 0),
            unspecified(),
            &current,
        )
        .unwrap();
    // Unmark is a metadata/group-completion action. Capture actual document and
    // mapping facts before it, then ensure no additional text transaction appears.
    let before = owner.state().unwrap();
    let source = document.borrow().original_backing_observer().unwrap();
    let original_mapping = source.mapped_bytes();
    let facts = {
        let document = document.borrow();
        (
            document.byte_len(),
            document.utf16_len(),
            document.generation(),
            document.can_undo(),
            document.can_redo(),
            document.is_dirty(),
        )
    };
    let permit = owner.capture(7, &current).unwrap();
    let cleared = committed(owner.unmark(&permit, &current).unwrap());
    assert_eq!(cleared.state.generation, before.generation);
    assert_eq!(cleared.state.selection, before.selection);
    assert_eq!(cleared.state.marked, None);
    assert!(cleared.invalidation.marked_changed);
    assert!(!cleared.invalidation.characters_changed);
    assert!(cleared.invalidation.replaced_utf16_range.is_none());
    assert_eq!(text(&document), "首中文🙂");
    let after = {
        let document = document.borrow();
        assert!(!document.has_prepared_text_input());
        (
            document.byte_len(),
            document.utf16_len(),
            document.generation(),
            document.can_undo(),
            document.can_redo(),
            document.is_dirty(),
        )
    };
    assert_eq!(facts, after);
    assert_eq!(source.mapped_bytes(), original_mapping);
    assert!(!source.unmap_succeeded() && !source.failed());
    let permit = owner.capture(7, &current).unwrap();
    committed(
        owner
            .history(&permit, NativeMarkdownPagedInputHistory::Undo, &current)
            .unwrap(),
    );
    assert_eq!(text(&document), "首"); // Unmark adds no empty history entry that would require a preliminary Undo.
    let permit = owner.capture(7, &current).unwrap();
    assert!(matches!(
        owner
            .history(&permit, NativeMarkdownPagedInputHistory::Undo, &current)
            .unwrap(),
        NativeMarkdownPagedInputOutcome::Unchanged(_)
    ));
}

#[test]
fn rejected_unicode_capacity_and_stale_permissions_keep_original_state() {
    let (_file, document) = fixture("😀", 8);
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let epoch = Cell::new(1);
    let current = |expected| expected == epoch.get();
    let before = owner.state().unwrap();
    // Relative marked selection must end on scalar boundaries, while replacement
    // endpoints and live-byte capacity are independently enforced by Core.
    let permit = owner.capture(1, &current).unwrap();
    assert!(matches!(
        owner.set_marked_text(&permit, "🙂", NSRange::new(1, 0), unspecified(), &current),
        Err(NativeMarkdownPagedInputError::InvalidMarkedSelection)
    ));
    assert_eq!(owner.state().unwrap(), before);
    let permit = owner.capture(1, &current).unwrap();
    assert!(
        owner
            .insert_text(&permit, "x", NSRange::new(1, 1), &current)
            .is_err()
    );
    assert_eq!(owner.state().unwrap(), before);
    let permit = owner.capture(1, &current).unwrap();
    assert!(
        owner
            .insert_text(&permit, "超出真实容量", unspecified(), &current)
            .is_err()
    );
    assert_eq!(owner.state().unwrap(), before);
    assert_eq!(text(&document), "😀");
    assert!(!document.borrow().can_undo());
    assert!(!document.borrow().has_prepared_text_input());
    let permit = owner.capture(1, &current).unwrap();
    epoch.set(2);
    assert!(matches!(
        owner.insert_text(&permit, "x", unspecified(), &current),
        Err(NativeMarkdownPagedInputError::Revoked)
    ));
    assert_eq!(owner.state().unwrap(), before);
}

#[test]
fn rejected_marked_replacement_preserves_the_existing_composition_and_history() {
    let (_file, document) = fixture("首", 12);
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let current = |_| true;
    let permit = owner.capture(1, &current).unwrap();
    owner
        .set_marked_text(&permit, "中🙂", NSRange::new(1, 2), unspecified(), &current)
        .unwrap();
    let before = owner.state().unwrap();
    let permit = owner.capture(1, &current).unwrap();
    assert!(
        owner
            .set_marked_text(
                &permit,
                "不能容纳的候选",
                NSRange::new(0, 0),
                unspecified(),
                &current,
            )
            .is_err()
    );
    // Capacity failure precedes candidate publication; failed input must retain the previous marked range and directed selection.
    assert_eq!(owner.state().unwrap(), before);
    assert_eq!(text(&document), "中🙂首");
    assert!(!document.borrow().has_prepared_text_input());
    let permit = owner.capture(1, &current).unwrap();
    committed(owner.unmark(&permit, &current).unwrap());
    let permit = owner.capture(1, &current).unwrap();
    committed(
        owner
            .history(&permit, NativeMarkdownPagedInputHistory::Undo, &current)
            .unwrap(),
    );
    assert_eq!(text(&document), "首"); // Failed replacement adds no history entry before or after the original composition.
}

#[test]
fn reentrant_permission_after_prepare_cancels_only_its_original_nonce() {
    let (_file, document) = fixture("原文", 4096);
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let revoked = Cell::new(false);
    let checks_during_mutation = Cell::new(0);
    let current = |epoch| {
        assert_eq!(epoch, 11);
        // The actual callback attempts document/fold borrows, checking that current never runs while those leases are held.
        let original = document.try_borrow_mut().expect("current跨了Doc借用");
        let folds = owner.folds.try_borrow_mut().expect("current跨了fold借用");
        if owner.is_busy() {
            checks_during_mutation.set(checks_during_mutation.get() + 1);
            assert!(matches!(
                owner.close(),
                Err(NativeMarkdownPagedInputError::Busy)
            ));
        }
        if original.has_prepared_text_input() {
            revoked.set(true);
        }
        drop(folds);
        drop(original);
        !revoked.get()
    };
    let before = owner.state().unwrap();
    let permit = owner.capture(11, &current).unwrap();
    assert!(matches!(
        owner.insert_text(&permit, "候选🙂", unspecified(), &current),
        Err(NativeMarkdownPagedInputError::Revoked)
    ));
    assert!(checks_during_mutation.get() > 0);
    assert_eq!(owner.state().unwrap(), before);
    assert_eq!(text(&document), "原文");
    assert!(!document.borrow().has_prepared_text_input());
    assert!(!document.borrow().can_undo());
    assert!(!owner.is_busy());
}

#[test]
fn submitted_receipt_revocation_is_separate_from_successful_edit() {
    let (_file, document) = fixture("原文", 4096);
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let allowed = Cell::new(true);
    let current = |_| allowed.get();
    let permit = owner.capture(3, &current).unwrap();
    let commit = owner
        .insert_text(&permit, "中🙂", unspecified(), &current)
        .unwrap();
    allowed.set(false);
    assert!(matches!(
        commit.check_current(&current),
        Err(NativeMarkdownPagedInputError::Revoked)
    ));
    assert_eq!(text(&document), "中🙂原文"); // Revoked presentation authority cannot report committed text as unwritten or replay it.
    assert!(document.borrow().can_undo());
}

#[test]
fn fold_anchor_preflight_and_publish_follow_the_actual_candidate_root() {
    let (_file, document) = fixture("# H\nbody\nend", 4096);
    let owner = NativeMarkdownPagedInput::with_folds(
        Rc::downgrade(&document),
        vec![MarkdownFoldRegion {
            header_line: 0,
            hidden_utf16_range: 3..8,
        }],
    )
    .unwrap();
    let current = |_| true;
    let permit = owner.capture(1, &current).unwrap();
    // A supplementary scalar adds two UTF-16 units. Both fold movement and Undo
    // must follow actual candidate coordinates rather than UTF-8 byte counts.
    let commit = owner
        .insert_text(&permit, "🙂", unspecified(), &current)
        .unwrap();
    assert!(commit.invalidation.folds_changed);
    assert_eq!(
        &*owner.folds.borrow(),
        &[MarkdownFoldRegion {
            header_line: 0,
            hidden_utf16_range: 5..10
        }]
    );
    let permit = owner.capture(1, &current).unwrap();
    committed(
        owner
            .history(&permit, NativeMarkdownPagedInputHistory::Undo, &current)
            .unwrap(),
    );
    assert_eq!(
        &*owner.folds.borrow(),
        &[MarkdownFoldRegion {
            header_line: 0,
            hidden_utf16_range: 3..8
        }]
    );
}

#[test]
fn input_permits_and_commits_do_not_keep_document_or_source_mapping_alive() {
    let (_file, document) = fixture("源🙂", 4096);
    let source = document.borrow().original_backing_observer().unwrap();
    let alias = document.borrow().text_snapshot();
    let weak = Rc::downgrade(&document);
    let owner = NativeMarkdownPagedInput::new(weak.clone()).unwrap();
    let current = |_| true;
    let permit = owner.capture(1, &current).unwrap();
    let commit = owner
        .insert_text(&permit, "中", unspecified(), &current)
        .unwrap();
    assert_eq!(Rc::strong_count(&document), 1);
    // Retain input/permit/receipt and one independent Core snapshot while dropping
    // the actual document owner; only that real snapshot may postpone unmapping.
    drop(document);
    assert!(weak.upgrade().is_none());
    assert!(matches!(
        owner.state(),
        Err(NativeMarkdownPagedInputError::DocumentReleased)
    ));
    assert!(!source.unmap_succeeded()); // An independent actual snapshot still owns the original mapping.
    drop(alias);
    assert!(source.unmap_succeeded());
    assert_eq!(source.mapped_bytes(), 0);
    assert!(!source.failed()); // Live input/permit/receipt objects retain no hidden Source payload.
    owner.close().unwrap();
    owner.close().unwrap();
    assert!(commit.check_current(&current).is_err());
    assert!(owner.folds.borrow().capacity() == 0);
}
