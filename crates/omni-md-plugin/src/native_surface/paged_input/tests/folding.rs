//! Exercise folding through production Input/Core with short synthetic text, checking canonical selection/history without a Native graph.

use super::*;

#[test]
// Folding changes bounded display anchors; a hidden head moves to the header using existing interaction and hides complete CRLF terminators.
fn source_fold_preserves_original_text_history_and_projects_only_real_line_terminators() {
    let original = "# A\r\none\r\ntwo\r\n# B\r\nend";
    let (_file, document) = fixture(original, 4096);
    document
        .borrow_mut()
        .set_selection(MarkdownTextSelection::caret(original.find("two").unwrap()))
        .unwrap();
    let input = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let before = input.state().unwrap();
    let current = |epoch| epoch == 7;
    let permit = input.capture(7, &current).unwrap();
    let folded = committed(input.toggle_fold(&permit, 0, &current).unwrap());
    assert_eq!(folded.state.generation, before.generation);
    assert_eq!(folded.state.selection, MarkdownTextSelection::caret(3));
    assert_eq!(folded.state.fold_revision, before.fold_revision + 1);
    assert!(!folded.invalidation.characters_changed && folded.invalidation.folds_changed);
    assert!(folded.invalidation.selection_changed);
    let permit = input.capture(7, &current).unwrap();
    assert_eq!(
        input
            .folded_layout_ranges(&permit, &current)
            .unwrap()
            .as_slice(),
        std::slice::from_ref(&(3..original.find("# B").unwrap()))
    );
    assert_eq!(text(&document), original);
    assert!(!document.borrow().can_undo() && !document.borrow().can_redo());
    let permit = input.capture(7, &current).unwrap();
    let expanded = committed(input.expand_folds(&permit, &current).unwrap());
    assert_eq!(expanded.state.selection, folded.state.selection);
    assert_eq!(expanded.state.generation, before.generation);
    assert!(!expanded.invalidation.selection_changed && expanded.invalidation.folds_changed);
    assert_eq!(text(&document), original);
    assert!(!document.borrow().can_undo() && !document.borrow().can_redo());
}

#[test]
// Reveal the entire Find match in one receipt so later geometry binds the new fold revision and retains hidden matches.
fn find_selection_unfolds_its_full_match_in_the_same_original_commit() {
    let original = "# A\nfirst\nneedle\n# B\nlast";
    let (_file, document) = fixture(original, 4096);
    let input = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let current = |epoch| epoch == 1;
    let permit = input.capture(1, &current).unwrap();
    let fold = committed(input.toggle_fold(&permit, 0, &current).unwrap());
    let start = original.find("needle").unwrap();
    let selected = MarkdownTextSelection {
        anchor_utf16: start,
        head_utf16: start + "needle".len(),
    };
    let permit = input.capture(1, &current).unwrap();
    let found = committed(
        input
            .select_find_result(&permit, selected, &current)
            .unwrap(),
    );
    assert_eq!(found.state.selection, selected);
    assert_eq!(found.state.fold_revision, fold.state.fold_revision + 1);
    assert!(found.invalidation.folds_changed && found.invalidation.selection_changed);
    assert!(!found.invalidation.characters_changed);
    assert_eq!(found.state.generation, fold.state.generation);
    let permit = input.capture(1, &current).unwrap();
    assert!(
        input
            .folded_layout_ranges(&permit, &current)
            .unwrap()
            .is_empty()
    );
    assert_eq!(text(&document), original);
    assert!(!document.borrow().can_undo() && !document.borrow().can_redo());
    let permit = input.capture(1, &current).unwrap();
    assert!(matches!(
        input.select_find_result(&permit, selected, &current).unwrap(),
        NativeMarkdownPagedInputOutcome::Unchanged(state) if state == found.state
    )); // Repeating the same visible match advances neither fold revision nor selection serial.
}

#[test]
// Revoked authority cannot fold another state; Expand All with no folds creates neither a display receipt nor Undo.
fn source_fold_rejects_revoked_authority_and_keeps_actual_noop_without_history() {
    let (_file, document) = fixture("# A\nbody", 4096);
    let input = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let before = input.state().unwrap();
    let permit = input.capture(1, &|_| true).unwrap();
    assert!(matches!(
        input.toggle_fold(&permit, 0, &|_| false),
        Err(NativeMarkdownPagedInputError::Revoked)
    ));
    assert_eq!(input.state().unwrap(), before);
    let permit = input.capture(1, &|_| true).unwrap();
    assert!(matches!(
        input.expand_folds(&permit, &|_| true).unwrap(),
        NativeMarkdownPagedInputOutcome::Unchanged(state) if state == before
    ));
    assert_eq!(text(&document), "# A\nbody");
    assert!(!document.borrow().can_undo() && !document.borrow().can_redo());
}

#[test]
// Programmatic selection cannot leave a caret hidden; selections spanning a fold with both endpoints outside retain it.
fn original_selection_into_hidden_source_unfolds_without_a_second_selection_receipt() {
    let original = "# A\nbody\n# B\nlast";
    let (_file, document) = fixture(original, 4096);
    let input = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let current = |epoch| epoch == 1;
    let permit = input.capture(1, &current).unwrap();
    let folded = committed(input.toggle_fold(&permit, 0, &current).unwrap());
    let permit = input.capture(1, &current).unwrap();
    let all = committed(
        input
            .set_selection(
                &permit,
                MarkdownTextSelection {
                    anchor_utf16: 0,
                    head_utf16: original.len(),
                },
                &current,
            )
            .unwrap(),
    );
    assert_eq!(all.state.fold_revision, folded.state.fold_revision);
    assert!(!all.invalidation.folds_changed);
    let permit = input.capture(1, &current).unwrap();
    let selected = committed(
        input
            .set_selection(&permit, MarkdownTextSelection::caret(4), &current)
            .unwrap(),
    );
    assert_eq!(selected.state.selection, MarkdownTextSelection::caret(4));
    assert_eq!(selected.state.fold_revision, all.state.fold_revision + 1);
    assert!(selected.invalidation.folds_changed && selected.invalidation.selection_changed);
    assert_eq!(selected.state.generation, folded.state.generation);
    assert_eq!(text(&document), original);
    assert!(!document.borrow().can_undo() && !document.borrow().can_redo());
}

#[test]
// The existing menu allows folding after Select All; external endpoints retain direction, serial, and text history.
fn folding_after_select_all_preserves_the_original_single_range_and_memento() {
    let original = "# A\nbody\n# B\nlast";
    let (_file, document) = fixture(original, 4096);
    let input = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let current = |epoch| epoch == 1;
    let permit = input.capture(1, &current).unwrap();
    let selected = committed(
        input
            .set_selection_intent(
                &permit,
                MarkdownTextSelection {
                    anchor_utf16: 0,
                    head_utf16: original.len(),
                },
                None,
                None,
                &current,
            )
            .unwrap(),
    );
    let permit = input.capture(1, &current).unwrap();
    let folded = committed(input.toggle_fold(&permit, 0, &current).unwrap());
    assert_eq!(folded.state.selection, selected.state.selection);
    assert_eq!(
        folded.state.selection_serial,
        selected.state.selection_serial
    );
    assert_eq!(folded.state.primary_range, selected.state.primary_range);
    assert_eq!(
        folded.state.selection_is_directed,
        selected.state.selection_is_directed
    );
    assert_eq!(folded.state.selection_range_count, 1);
    assert!(!folded.invalidation.selection_changed);
    assert!(folded.invalidation.folds_changed);
    assert_eq!(folded.state.generation, selected.state.generation);
    assert_eq!(text(&document), original);
    assert!(!document.borrow().can_undo() && !document.borrow().can_redo());
}
