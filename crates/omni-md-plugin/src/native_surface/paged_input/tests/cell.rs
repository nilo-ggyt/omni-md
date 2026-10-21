//! Exercise production Cell targets and verified tables in the same document, without NSTextField or a second draft document.

use super::*;
use omni_markdown_core::markdown_document_buffer::{
    MarkdownTableWindowStep, MarkdownTableWindowTarget,
};
use omni_markdown_core::{
    MarkdownCellDraftCommitError, MarkdownCellDraftError, MarkdownCellDraftHistoryFacts,
};

const SOURCE: &str = "| H | C |\n| --- | --- |\n| base | other |\n";
type CellFixture = (
    tempfile::NamedTempFile,
    Rc<RefCell<MarkdownDocumentBuffer>>,
    Rc<NativeMarkdownPagedCellSession>,
    Rc<NativeMarkdownPagedInput>,
);

fn cell_fixture() -> CellFixture {
    // Acquire the same verified finite table window/cell authority as production;
    // a raw row/column tuple cannot start a local editing session by itself.
    let (file, document) = fixture(SOURCE, 32 * 1024 * 1024);
    let verified = {
        let document = document.borrow();
        let mut request = document
            .table_window_request(
                MarkdownTableWindowTarget::AtUtf16(0),
                0..document.utf16_len(),
                1,
                0..0,
            )
            .unwrap();
        loop {
            match request.advance(&document, false, 4096).unwrap() {
                MarkdownTableWindowStep::Progress { .. } => {}
                MarkdownTableWindowStep::Ready { .. } => {
                    break request.take_ready_for(&document).unwrap().unwrap();
                }
                other => panic!("实际有限窗口未Ready：{other:?}"),
            }
        }
    };
    let source = document
        .borrow()
        .verified_table_cell_source(&verified, 1, 0)
        .unwrap();
    let session = NativeMarkdownPagedCellSession::begin(Rc::downgrade(&document), source).unwrap();
    let input = NativeMarkdownPagedInput::new_cell(Rc::downgrade(&session)).unwrap();
    (file, document, session, input)
}

fn local_text(input: &Rc<NativeMarkdownPagedInput>) -> String {
    // Exercise the public target-aware reader. A Cell input intentionally has no
    // canonical document Weak that could reinterpret local positions as Body.
    let permit = input.capture(1, &|_| true).unwrap();
    let length = input.state().unwrap().utf16_len;
    assert!(length <= 256); // Materialize only the small oracle; inspect 2 MiB through fixed chunks without adding a production read backdoor.
    let mut units = [0; 256];
    input
        .copy_utf16_code_units(&permit, 0, &mut units[..length], &|_| true)
        .unwrap();
    String::from_utf16(&units[..length]).unwrap()
}

fn select(input: &Rc<NativeMarkdownPagedInput>, selection: MarkdownTextSelection) {
    // Each actual input action receives a fresh one-shot permit; selection changes
    // therefore invalidate earlier Cell capsules through real Core serials.
    let permit = input.capture(1, &|_| true).unwrap();
    input.set_selection(&permit, selection, &|_| true).unwrap();
}

fn insert(input: &Rc<NativeMarkdownPagedInput>, value: &str) {
    // NSNotFound delegates the marked/selection precedence to production Input.
    let permit = input.capture(1, &|_| true).unwrap();
    input
        .insert_text(&permit, value, unspecified(), &|_| true)
        .unwrap();
}

#[test]
fn actual_cell_ime_and_typing_groups_do_not_publish_canonical_and_match_measured_selections() {
    for ime in [true, false] {
        let (_file, document, session, input) = cell_fixture();
        // Toolbar availability follows the actual local stack, not whether canonical history happens to contain Undo.
        assert_eq!(
            input.cell_history_facts().unwrap(),
            Some(MarkdownCellDraftHistoryFacts {
                can_undo: false,
                can_redo: false,
            })
        );
        if ime {
            // Candidate replacements and confirmation belong to one composition
            // group while preserving the original selected range for Undo.
            select(
                &input,
                MarkdownTextSelection {
                    anchor_utf16: 0,
                    head_utf16: 4,
                },
            );
            for candidate in ["n", "ni"] {
                let permit = input.capture(1, &|_| true).unwrap();
                input
                    .set_marked_text(
                        &permit,
                        candidate,
                        NSRange::new(candidate.len(), 0),
                        unspecified(),
                        &|_| true,
                    )
                    .unwrap();
            }
            insert(&input, "你");
        } else {
            // Adjacent ordinary insertions coalesce in the draft's real local
            // history without publishing any canonical transaction.
            select(&input, MarkdownTextSelection::caret(4));
            for candidate in ["a", "b", "c"] {
                insert(&input, candidate);
            }
        }
        assert_eq!(
            input.cell_history_facts().unwrap(),
            Some(MarkdownCellDraftHistoryFacts {
                can_undo: true,
                can_redo: false,
            })
        );
        let permit = input.capture(1, &|_| true).unwrap();
        let commit = committed(
            input
                .history(&permit, NativeMarkdownPagedInputHistory::Undo, &|_| true)
                .unwrap(),
        );
        assert_eq!(local_text(&input), "base");
        assert_eq!(
            commit.state.selection,
            if ime {
                MarkdownTextSelection {
                    anchor_utf16: 0,
                    head_utf16: 4,
                }
            } else {
                MarkdownTextSelection::caret(4)
            }
        );
        assert!(commit.state.marked.is_none());
        // One continuous group needs one Undo; an exhausted local stack disables the menu without falling through to Body Undo.
        assert_eq!(
            input.cell_history_facts().unwrap(),
            Some(MarkdownCellDraftHistoryFacts {
                can_undo: false,
                can_redo: true,
            })
        );
        let permit = input.capture(1, &|_| true).unwrap();
        assert!(matches!(
            input
                .history(&permit, NativeMarkdownPagedInputHistory::Undo, &|_| true)
                .unwrap(),
            NativeMarkdownPagedInputOutcome::Unchanged(_)
        ));
        let permit = input.capture(1, &|_| true).unwrap();
        let redo = committed(
            input
                .history(&permit, NativeMarkdownPagedInputHistory::Redo, &|_| true)
                .unwrap(),
        );
        assert_eq!(local_text(&input), if ime { "你" } else { "baseabc" });
        assert_eq!(
            redo.state.selection,
            MarkdownTextSelection::caret(if ime { 1 } else { 7 })
        );
        assert_eq!(text(&document), SOURCE);
        input.close().unwrap();
        session.close().unwrap();
        assert!(!document.borrow().has_cell_draft());
    }
}

#[test]
fn actual_cell_selection_and_explicit_break_keep_two_original_redo_endpoints() {
    for moved in [false, true] {
        let (_file, document, session, input) = cell_fixture();
        select(&input, MarkdownTextSelection::caret(4));
        insert(&input, "a");
        insert(&input, "b");
        if moved {
            // An explicit selection move and breakUndoCoalescing are distinct
            // public boundaries; either must keep both original Redo endpoints.
            select(&input, MarkdownTextSelection::caret(0));
        } else {
            let permit = input.capture(1, &|_| true).unwrap();
            input.break_undo_coalescing(&permit, &|_| true).unwrap();
        }
        insert(&input, if moved { "x" } else { "c" });
        for (expected, caret) in [("baseab", if moved { 0 } else { 6 }), ("base", 4)] {
            let permit = input.capture(1, &|_| true).unwrap();
            let undo = committed(
                input
                    .history(&permit, NativeMarkdownPagedInputHistory::Undo, &|_| true)
                    .unwrap(),
            );
            assert_eq!(local_text(&input), expected);
            assert_eq!(undo.state.selection, MarkdownTextSelection::caret(caret));
        }
        for (expected, caret) in [
            ("baseab", 6),
            (
                if moved { "xbaseab" } else { "baseabc" },
                if moved { 1 } else { 7 },
            ),
        ] {
            let permit = input.capture(1, &|_| true).unwrap();
            let redo = committed(
                input
                    .history(&permit, NativeMarkdownPagedInputHistory::Redo, &|_| true)
                    .unwrap(),
            );
            assert_eq!(local_text(&input), expected);
            assert_eq!(redo.state.selection, MarkdownTextSelection::caret(caret));
        }
        assert_eq!(text(&document), SOURCE);
        input.close().unwrap();
        session.close().unwrap();
    }
}

#[test]
fn two_real_scene_readers_do_not_block_finite_input_reads_or_create_a_third_reader() {
    let (_file, document, session, input) = cell_fixture();
    let first = session.capture_source().unwrap();
    let second = session.capture_source().unwrap();
    let observer = session.resource_observer().unwrap();
    let before = observer.snapshot();
    assert!(matches!(
        session.capture_source(),
        Err(NativeMarkdownPagedInputError::Cell(
            MarkdownCellDraftError::ReadersBusy
        ))
    ));
    let permit = input.capture(1, &|_| true).unwrap();
    let mut units = [0; 4];
    input
        .copy_utf16_code_units(&permit, 0, &mut units, &|_| true)
        .unwrap();
    input
        .validate_utf16_range(&permit, 0..4, &|_| true)
        .unwrap();
    assert_eq!(String::from_utf16(&units).unwrap(), "base");
    assert_eq!(observer.snapshot(), before);
    // Cancellation revokes the session but cannot release resources while actual
    // Source readers remain; verify release only after Drop and the real drain.
    input.close().unwrap();
    session.close().unwrap();
    assert!(!observer.is_released());
    drop(first);
    drop(second);
    document.borrow_mut().drain_cancelled_cell_draft();
    assert!(observer.is_released());
}

#[test]
fn cell_domain_rejects_canonical_and_stale_content_but_selection_keeps_same_source() {
    let (_file, document, session, input) = cell_fixture();
    assert!(input.document_identity().upgrade().is_none());
    let domain = input.target_domain().unwrap();
    let source = session.capture_source().unwrap();
    domain.require_source(&source).unwrap();
    let canonical = omni_markdown_core::MarkdownLayoutTextSource::capture(&document.borrow());
    assert!(domain.require_source(&canonical).is_err());
    select(&input, MarkdownTextSelection::caret(4));
    assert!(domain.same(&input.target_domain().unwrap()));
    insert(&input, "中😀");
    assert!(!domain.same(&input.target_domain().unwrap()));
    assert!(domain.require_source(&source).is_err());
    let permit = input.capture(1, &|_| true).unwrap();
    // Code-unit reading permits the low half of a pair for NSString primitives;
    // scalar-range validation must independently reject that same position.
    let mut low = [0; 1];
    input
        .copy_utf16_code_units(&permit, 6, &mut low, &|_| true)
        .unwrap();
    assert!((0xdc00..=0xdfff).contains(&low[0]));
    assert!(
        input
            .validate_utf16_range(&permit, 6..6, &|_| true)
            .is_err()
    );
    input.close().unwrap();
    session.close().unwrap();
    drop(source);
    drop(canonical);
    document.borrow_mut().drain_cancelled_cell_draft();
}

#[test]
/// Read-only cells and active drafts in the same document cannot substitute local UTF-16 for Body coordinates or share input authority.
fn canonical_and_active_draft_domains_both_reject_readonly_raw_cell_sources() {
    use omni_markdown_core::{MarkdownLayoutTextSource, MarkdownLayoutTextSourceDomain};
    let (_file, document, session, input) = cell_fixture();
    let mut request = document.borrow().table_cell_edit_request(0, 1, 0).unwrap();
    let cell = loop {
        let document = document.borrow();
        match request.advance(&document, false, 4096).unwrap() {
            omni_markdown_core::markdown_document_buffer::MarkdownTableCellEditStep::Progress {
                ..
            } => {}
            omni_markdown_core::markdown_document_buffer::MarkdownTableCellEditStep::Ready {
                ..
            } => {
                break request.take_cell_source_for(&document).unwrap().unwrap();
            }
            other => panic!("原格尚未取得结构来源：{other:?}"),
        }
    };
    let raw = MarkdownLayoutTextSource::capture_table_cell(&document.borrow(), &cell).unwrap();
    let canonical = MarkdownLayoutTextSource::capture(&document.borrow());
    let draft = session.capture_source().unwrap();
    let body = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let body_domain = body.target_domain().unwrap();
    let cell_domain = input.target_domain().unwrap();
    assert_eq!(raw.domain(), MarkdownLayoutTextSourceDomain::ReadOnlyCell);
    assert_eq!(
        canonical.domain(),
        MarkdownLayoutTextSourceDomain::Canonical
    );
    assert_eq!(
        draft.domain(),
        MarkdownLayoutTextSourceDomain::ActiveCellDraft
    );
    body_domain.require_source(&canonical).unwrap();
    cell_domain.require_source(&draft).unwrap();
    for rejected in [&raw, &draft] {
        assert!(matches!(
            body_domain.require_source(rejected),
            Err(NativeMarkdownPagedInputError::StateChanged)
        ));
    }
    for rejected in [&raw, &canonical] {
        assert!(matches!(
            cell_domain.require_source(rejected),
            Err(NativeMarkdownPagedInputError::StateChanged)
        ));
    }
    body.close().unwrap();
    input.close().unwrap();
    session.close().unwrap();
    drop(draft); // Return the actual draft reader; the read-only cell owns neither that reader slot nor the session cancellation boundary.
    document.borrow_mut().drain_cancelled_cell_draft();
    raw.byte_offset_for_utf16(4).unwrap();
}

#[test]
fn same_original_capsule_cannot_be_rebound_after_external_state_change_or_consumed_twice() {
    let (_file, _document, session, input) = cell_fixture();
    let permit = input.capture(1, &|_| true).unwrap();
    input
        .insert_text(&permit, "x", unspecified(), &|_| true)
        .unwrap();
    assert!(matches!(
        input.insert_text(&permit, "y", unspecified(), &|_| true),
        Err(NativeMarkdownPagedInputError::Consumed)
    ));
    let permit = input.capture(1, &|_| true).unwrap();
    // A second actual Input publishes selection through the same session, making
    // this original capsule stale without changing any private field in the test.
    let other = NativeMarkdownPagedInput::new_cell(Rc::downgrade(&session)).unwrap();
    let foreign = other.capture(1, &|_| true).unwrap();
    other
        .set_selection(&foreign, MarkdownTextSelection::caret(0), &|_| true)
        .unwrap();
    assert!(
        input
            .insert_text(&permit, "z", unspecified(), &|_| true)
            .is_err()
    );
    input.close().unwrap();
    other.close().unwrap();
    session.close().unwrap();
}

#[test]
fn native_conversion_preserves_typed_draft_revocation_instead_of_replacing_it_with_text_error() {
    let (_file, document, session, input) = cell_fixture();
    let resources = session.resource_observer().unwrap();
    let permit = input.capture(1, &|_| true).unwrap();
    let changed = Cell::new(false);
    let current = |_| {
        // begin validates before busy; revoke the session in the actual conversion guard, which must run after every borrow is returned.
        if input.is_busy() && !changed.replace(true) {
            session.close().unwrap();
            false
        } else {
            true
        }
    };
    let native = objc2_foundation::NSString::from_str("原生新增");
    assert!(matches!(
        input.insert_native(&permit, &native, unspecified(), &current),
        Err(NativeMarkdownPagedInputError::Cell(
            MarkdownCellDraftError::Stale
        ))
    ));
    assert!(changed.get());
    assert!(resources.is_released());
    assert_eq!(text(&document), SOURCE);
    input.close().unwrap();
}

#[test]
fn prepared_commit_keeps_scene_until_admission_and_consumes_once_after_real_readers_drop() {
    let (_file, document, session, input) = cell_fixture();
    select(
        &input,
        MarkdownTextSelection {
            anchor_utf16: 0,
            head_utf16: 4,
        },
    );
    insert(&input, "中|😀");
    let first = session.capture_source().unwrap();
    let second = session.capture_source().unwrap();
    let observer = session.resource_observer().unwrap();
    // Commit through the existing production Body Input so its actual FoldObserver
    // prepares/publishes canonical selection and folds instead of a test observer.
    let body = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let permit = input.capture(1, &|_| true).unwrap();
    let prepared = input
        .prepare_cell_commit(&permit, None, None, &|_| true)
        .unwrap();
    assert!(observer.snapshot().prepared_commit_live);
    let body_permit = body.capture(1, &|_| true).unwrap();
    assert!(matches!(
        input.commit_cell_to_body(prepared, &body, &body_permit, &|_| true, &|_| true),
        Err(NativeMarkdownPagedCellCommitError::Commit(
            MarkdownCellDraftCommitError::Draft(MarkdownCellDraftError::ReadersBusy)
        ))
    ));
    assert_eq!(local_text(&input), "中|😀");
    assert!(!observer.snapshot().prepared_commit_live);
    let permit = input.capture(1, &|_| true).unwrap();
    let prepared = input
        .prepare_cell_commit(&permit, None, None, &|_| true)
        .unwrap();
    drop(first);
    drop(second);
    let body_permit = body.capture(1, &|_| true).unwrap();
    assert!(matches!(
        input
            .commit_cell_to_body(prepared, &body, &body_permit, &|_| true, &|_| true)
            .unwrap()
            .outcome,
        NativeMarkdownPagedInputOutcome::Committed(_)
    ));
    assert!(observer.is_released());
    assert!(document.borrow().can_undo());
    document.borrow_mut().undo().unwrap();
    assert_eq!(text(&document), SOURCE);
    body.close().unwrap();
}

#[test]
fn two_mebibyte_raw_commit_waits_for_original_readers_and_preserves_one_undo() {
    let (_file, document, session, input) = cell_fixture();
    select(
        &input,
        MarkdownTextSelection {
            anchor_utf16: 0,
            head_utf16: 4,
        },
    );
    let added = "x".repeat(2 * 1024 * 1024); // This is newly supplied input only; Core stores appended pieces, not this test String.
    insert(&input, &added);
    drop(added);
    let first = session.capture_source().unwrap();
    let second = session.capture_source().unwrap();
    let state = input.state().unwrap();
    assert_eq!(state.utf16_len, 2 * 1024 * 1024);
    let observer = session.resource_observer().unwrap();
    // Preserve the 2 MiB streaming/reader regression while using the real canonical
    // Input transaction and its FoldObserver for both rejection and publication.
    let body = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let permit = input.capture(1, &|_| true).unwrap();
    let prepared = input
        .prepare_cell_commit(&permit, None, None, &|_| true)
        .unwrap();
    // Large output may prepare by streaming; live Scene readers must still block publication and return that admission output.
    assert!(observer.snapshot().prepared_commit_live);
    let body_permit = body.capture(1, &|_| true).unwrap();
    assert!(matches!(
        input.commit_cell_to_body(prepared, &body, &body_permit, &|_| true, &|_| true),
        Err(NativeMarkdownPagedCellCommitError::Commit(
            MarkdownCellDraftCommitError::Draft(MarkdownCellDraftError::ReadersBusy)
        ))
    ));
    assert_eq!(input.state().unwrap(), state);
    assert!(!observer.snapshot().prepared_commit_live);
    let permit = input.capture(1, &|_| true).unwrap();
    let mut units = [0; 4096];
    for offset in [0, 1024 * 1024, state.utf16_len - 4096] {
        input
            .copy_utf16_code_units(&permit, offset, &mut units, &|_| true)
            .unwrap();
        assert!(units.iter().all(|unit| *unit == u16::from(b'x')));
    }
    assert_eq!(text(&document), SOURCE);
    // After the original readers actually end, commit the same draft through one canonical transaction.
    let prepared = input
        .prepare_cell_commit(&permit, None, None, &|_| true)
        .unwrap();
    drop(first);
    drop(second);
    let body_permit = body.capture(1, &|_| true).unwrap();
    assert!(matches!(
        input
            .commit_cell_to_body(prepared, &body, &body_permit, &|_| true, &|_| true)
            .unwrap()
            .outcome,
        NativeMarkdownPagedInputOutcome::Committed(_)
    ));
    assert!(observer.is_released());
    let start = SOURCE.find("base").unwrap();
    let length = state.utf16_len;
    for replay in [false, true] {
        // Verify the entire inserted range in 4 Ki UTF-16 chunks and fixed prefix/suffix separately, without materializing 2 MiB output.
        let doc = document.borrow();
        assert_eq!(doc.byte_len(), SOURCE.len() - 4 + length);
        assert_eq!(doc.utf16_len(), SOURCE.len() - 4 + length);
        let mut context = [0; 64];
        doc.copy_utf16_code_units(0, &mut context[..start]).unwrap();
        assert!(
            context[..start]
                .iter()
                .copied()
                .eq(SOURCE.bytes().take(start).map(u16::from))
        );
        for offset in (0..length).step_by(units.len()) {
            doc.copy_utf16_code_units(start + offset, &mut units)
                .unwrap();
            assert!(units.iter().all(|unit| *unit == u16::from(b'x')));
        }
        let suffix = &SOURCE.as_bytes()[start + 4..];
        doc.copy_utf16_code_units(start + length, &mut context[..suffix.len()])
            .unwrap();
        assert!(
            context[..suffix.len()]
                .iter()
                .copied()
                .eq(suffix.iter().copied().map(u16::from))
        );
        assert!(doc.can_undo());
        assert!(!doc.can_redo());
        drop(doc);
        if !replay {
            // One Undo restores exact original text without empty history; one Redo restores the same large output.
            document.borrow_mut().undo().unwrap();
            assert_eq!(text(&document), SOURCE);
            assert!(!document.borrow().can_undo());
            assert!(document.borrow().can_redo());
            document.borrow_mut().redo().unwrap();
        }
    }
    input.close().unwrap();
    session.close().unwrap();
    body.close().unwrap();
}

#[test]
fn input_and_old_domain_are_weak_and_do_not_keep_cell_session_or_document_alive() {
    let (_file, document, session, input) = cell_fixture();
    let source = session.capture_source().unwrap();
    let domain = input.target_domain().unwrap();
    let observer = session.resource_observer().unwrap();
    let session_weak = Rc::downgrade(&session);
    let doc_weak = Rc::downgrade(&document);
    // Keep domain/Input/observer alive after dropping the sole session owner.
    // Only an actual Source reader may hold the retired branch until it returns.
    drop(session);
    assert!(session_weak.upgrade().is_none());
    assert!(input.state().is_err() && domain.require_source(&source).is_err());
    drop(source);
    document.borrow_mut().drain_cancelled_cell_draft();
    assert!(observer.is_released());
    drop(document);
    assert!(doc_weak.upgrade().is_none());
    input.close().unwrap();
}
