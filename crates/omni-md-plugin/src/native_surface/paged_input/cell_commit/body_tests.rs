//! Real Cell admission consumes original Body authority without NSApplication or a no-op replacement for fold publication.

use super::*;
use omni_markdown_core::markdown_document_buffer::{
    MarkdownTableWindowStep, MarkdownTableWindowTarget, MarkdownViewportSourceRequest,
    MarkdownViewportSourceRequiredRowResult, VerifiedMarkdownViewportSources,
};
use omni_markdown_core::markdown_viewport_presentation::table_window::navigation::MarkdownTableWindowNavigation;

const SOURCE: &str = "| H | C |\n| --- | --- |\n| base | other |\n# H\nbody\nend";
const CELL_EPOCH: u64 = 7;
const BODY_EPOCH: u64 = 11;

struct Fixture {
    _file: tempfile::NamedTempFile,
    document: Rc<RefCell<MarkdownDocumentBuffer>>,
    session: Rc<NativeMarkdownPagedCellSession>,
    cell: Rc<NativeMarkdownPagedInput>,
    body: Rc<NativeMarkdownPagedInput>,
    fold: MarkdownFoldRegion,
}

impl Fixture {
    fn new() -> Self {
        Self::with_stale_body_permit(false, 0).0
    }

    /// Create stale authority before Cell admission; canonical selection mutation while a draft is live is itself forbidden by the tested boundary.
    fn with_stale_body_permit(
        stale: bool,
        column: usize,
    ) -> (Self, Option<NativeMarkdownPagedInputPermit>) {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), SOURCE).unwrap();
        let document = Rc::new(RefCell::new(
            MarkdownDocumentBuffer::open(file.path(), 1024 * 1024).unwrap(),
        ));
        let offset = SOURCE.find("# H").unwrap();
        let fold = MarkdownFoldRegion {
            header_line: {
                let original = document.borrow();
                let byte = original.byte_offset_for_utf16(offset).unwrap();
                original.line_for_byte(byte).unwrap()
            },
            hidden_utf16_range: offset + 3..offset + 8,
        };
        let body =
            NativeMarkdownPagedInput::with_folds(Rc::downgrade(&document), vec![fold.clone()])
                .unwrap();
        // Capture original Body authority, then publish selection through another actual Body permit before beginning the draft.
        // The allowed public operation advances actual serial and revokes the old permit without mutating private fields.
        let stale_permit = if stale {
            let old = body.capture(BODY_EPOCH, &|_| true).unwrap();
            let fresh = body.capture(BODY_EPOCH, &|_| true).unwrap();
            body.set_selection(&fresh, MarkdownTextSelection::caret(1), &|_| true)
                .unwrap();
            Some(old)
        } else {
            None
        };
        let verified = {
            let original = document.borrow();
            let mut request = original
                .table_window_request(
                    MarkdownTableWindowTarget::AtUtf16(0),
                    0..original.utf16_len(),
                    1,
                    0..0,
                )
                .unwrap();
            loop {
                match request.advance(&original, false, 4096).unwrap() {
                    MarkdownTableWindowStep::Progress { .. } => {}
                    MarkdownTableWindowStep::Ready { .. } => {
                        break request.take_ready_for(&original).unwrap().unwrap();
                    }
                    _ => panic!("原有限表格窗口未Ready"),
                }
            }
        };
        let source = document
            .borrow()
            .verified_table_cell_source(&verified, 1, column)
            .unwrap();
        let session =
            NativeMarkdownPagedCellSession::begin(Rc::downgrade(&document), source).unwrap();
        let cell = NativeMarkdownPagedInput::new_cell(Rc::downgrade(&session)).unwrap();
        (
            Self {
                _file: file,
                document,
                session,
                cell,
                body,
                fold,
            },
            stale_permit,
        )
    }
    fn replace(&self, value: &str) {
        let length = self.cell.state().unwrap().utf16_len;
        let permit = self
            .cell
            .capture(CELL_EPOCH, &|epoch| epoch == CELL_EPOCH)
            .unwrap();
        self.cell
            .insert_text(&permit, value, NSRange::new(0, length), &|epoch| {
                epoch == CELL_EPOCH
            })
            .unwrap();
    }
    fn prepare(&self) -> NativeMarkdownPagedCellPreparedCommit {
        let permit = self
            .cell
            .capture(CELL_EPOCH, &|epoch| epoch == CELL_EPOCH)
            .unwrap();
        self.cell
            .prepare_cell_commit(&permit, None, None, &|epoch| epoch == CELL_EPOCH)
            .unwrap()
    }
    fn body_permit(&self) -> NativeMarkdownPagedInputPermit {
        self.body
            .capture(BODY_EPOCH, &|epoch| epoch == BODY_EPOCH)
            .unwrap()
    }
    fn text(&self) -> String {
        let original = self.document.borrow();
        assert!(original.utf16_len() < 256); // Only a bounded oracle; the production bridge never captures full canonical text.
        let mut units = [0; 256];
        let length = original.utf16_len();
        original
            .copy_utf16_code_units(0, &mut units[..length])
            .unwrap();
        String::from_utf16(&units[..length]).unwrap()
    }
}

/// Advance the same production request with its 4096-work quantum, without another parser or authority invented from a row number.
fn finish_sources(
    request: &mut MarkdownViewportSourceRequest,
    document: &MarkdownDocumentBuffer,
    authority: MarkdownViewportSourceAuthority,
) -> VerifiedMarkdownViewportSources {
    loop {
        request.advance(document, authority, 4096).unwrap();
        if let Some(sources) = request.take_sources_for(document, authority).unwrap() {
            return sources;
        }
    }
}

#[test]
/// Admit the last-cell draft and actual terminal under one Input; after both readers return, publish cell+append once.
fn real_terminal_append_to_existing_body_preserves_directed_receipt_and_one_undo() {
    let fixture = Fixture::with_stale_body_permit(false, 1).0;
    let original_mapping = fixture
        .document
        .borrow()
        .original_backing_observer()
        .unwrap();
    let (mut request, terminal, authority) = {
        let document = fixture.document.borrow();
        let authority = MarkdownViewportSourceAuthority {
            context_revision: 73,
            selection: document.selection(),
            cancelled: false,
        };
        let mut request = document
            .viewport_leaf_source_request(0..1, 73, document.selection())
            .unwrap();
        let mut sources = finish_sources(&mut request, &document, authority);
        let mut row = sources
            .table_row_at_for(&document, authority, 0)
            .unwrap()
            .unwrap();
        // Walk from the verified header; only the original request's End authorizes append, without preparing the entire offscreen table.
        let terminal = loop {
            let query = sources
                .required_row_query_for(
                    &document,
                    authority,
                    &row,
                    MarkdownTableWindowNavigation::Next,
                )
                .unwrap()
                .unwrap();
            request
                .continue_forward_with_required_row(&document, &sources, 0..1, authority, query)
                .unwrap();
            sources = finish_sources(&mut request, &document, authority);
            match sources
                .required_row_result_for(&document, authority)
                .unwrap()
                .unwrap()
            {
                MarkdownViewportSourceRequiredRowResult::Row { row: next } => row = next,
                MarkdownViewportSourceRequiredRowResult::TableEnd { terminal } => break terminal,
            }
        };
        (request, terminal, authority)
    };
    fixture.replace("  新|😀 \u{2003}");
    let permit = fixture.cell.capture(CELL_EPOCH, &|_| true).unwrap();
    fixture
        .cell
        .set_selection(
            &permit,
            MarkdownTextSelection {
                anchor_utf16: 8,
                head_utf16: 3,
            },
            &|_| true,
        )
        .unwrap();
    let resources = fixture.session.resource_observer().unwrap();
    let first = fixture.session.capture_source().unwrap();
    let second = fixture.session.capture_source().unwrap();
    let permit = fixture.cell.capture(CELL_EPOCH, &|_| true).unwrap();
    let prepared = fixture
        .cell
        .prepare_cell_commit(
            &permit,
            Some((&terminal, authority)),
            Some(MarkdownCellDraftWordAnchor {
                range: MarkdownTextSelection {
                    anchor_utf16: 2,
                    head_utf16: 6,
                },
                origin_utf16: 4,
            }),
            &|_| true,
        )
        .unwrap();
    assert_eq!(prepared.next_cell(), Some((2, 0)));
    assert_eq!(fixture.text(), SOURCE); // Preparation retains display/disk text; do not commit the draft before a separate append transaction.
    drop((first, second));
    let body_permit = fixture.body_permit();
    let result = fixture
        .cell
        .commit_cell_to_body(prepared, &fixture.body, &body_permit, &|_| true, &|_| true)
        .unwrap();
    let NativeMarkdownPagedInputOutcome::Committed(commit) = result.outcome else {
        panic!("真实追加必须有原Body提交结果")
    };
    commit.check_current(&|_| true).unwrap();
    let start = SOURCE.find("other").unwrap();
    assert_eq!(
        result.receipt.selection,
        MarkdownTextSelection {
            anchor_utf16: start + 5,
            head_utf16: start + 1,
        }
    );
    assert_eq!(result.receipt.next_cell, Some((2, 0)));
    assert_eq!(
        result.receipt.word_anchor,
        Some(MarkdownCellDraftWordAnchor {
            range: MarkdownTextSelection {
                anchor_utf16: start,
                head_utf16: start + 5
            },
            origin_utf16: start + 3
        })
    ); // Map both word endpoints and the initial hit through trimming/pipe escaping; do not re-expand a word from bare postcommit positions.
    assert!(resources.is_released());
    let after = SOURCE.replacen("other |\n", "新\\|😀 |\n| | |\n", 1);
    assert_eq!(fixture.text(), after);
    assert_eq!(
        std::fs::read(fixture.document.borrow().path()).unwrap(),
        SOURCE.as_bytes()
    );
    // One canonical history transaction covers the append; old input cannot append again, and one Undo restores exact disk text.
    let permit = fixture.body_permit();
    fixture
        .body
        .history(&permit, NativeMarkdownPagedInputHistory::Undo, &|_| true)
        .unwrap();
    assert_eq!(fixture.text(), SOURCE);
    assert!(!fixture.document.borrow().can_undo());
    let permit = fixture.body_permit();
    fixture
        .body
        .history(&permit, NativeMarkdownPagedInputHistory::Redo, &|_| true)
        .unwrap();
    assert_eq!(fixture.text(), after);
    let mappings: Vec<_> = fixture.document.borrow().backing_observers().collect();
    request.cancel();
    drop((terminal, request, fixture));
    assert!(original_mapping.unmap_succeeded() && !original_mapping.failed());
    assert!(
        mappings
            .iter()
            .all(|mapping| mapping.unmap_succeeded() && !mapping.failed())
    );
}

#[test]
fn real_cell_commit_updates_existing_body_folds_global_selection_and_one_undo_redo() {
    let fixture = Fixture::new();
    let before = fixture.body.state().unwrap();
    fixture.replace("新中|😀");
    let permit = fixture.cell.capture(CELL_EPOCH, &|_| true).unwrap();
    fixture
        .cell
        .set_selection(
            &permit,
            MarkdownTextSelection {
                anchor_utf16: 5,
                head_utf16: 2,
            },
            &|_| true,
        )
        .unwrap();
    let resources = fixture.session.resource_observer().unwrap();
    let body_permit = fixture.body_permit();
    let outcome = fixture
        .cell
        .commit_cell_to_body(
            fixture.prepare(),
            &fixture.body,
            &body_permit,
            &|epoch| epoch == CELL_EPOCH,
            &|epoch| epoch == BODY_EPOCH,
        )
        .unwrap();
    let NativeMarkdownPagedInputOutcome::Committed(commit) = outcome.outcome else {
        panic!("真实修改必须返回Body commit");
    };
    // Body publishes its original after-caret; separate actual endpoints retain draft direction for later Shift use under original authority.
    let start = SOURCE.find("base").unwrap();
    assert_eq!(
        outcome.receipt.selection,
        MarkdownTextSelection {
            anchor_utf16: start + 6,
            head_utf16: start + 2,
        }
    );
    assert_eq!(outcome.receipt.next_cell, None);
    assert!(Weak::ptr_eq(&commit.owner, &Rc::downgrade(&fixture.body)));
    assert_eq!(commit.state, fixture.body.state().unwrap());
    assert_eq!(commit.state.generation, before.generation + 1);
    assert_eq!(commit.invalidation.change_in_utf16_length, 2);
    assert!(commit.invalidation.characters_changed && commit.invalidation.folds_changed);
    assert_eq!(
        commit.state.selection,
        fixture.document.borrow().selection()
    );
    assert_eq!(fixture.text(), SOURCE.replacen("base", "新中\\|😀", 1));
    assert_eq!(
        fixture.body.folds.borrow()[0].hidden_utf16_range,
        fixture.fold.hidden_utf16_range.start + 2..fixture.fold.hidden_utf16_range.end + 2
    );
    assert!(fixture.cell.closed.get() && fixture.session.facts().is_err());
    assert!(resources.is_released());
    commit.check_current(&|epoch| epoch == BODY_EPOCH).unwrap();
    assert!(matches!(
        fixture
            .body
            .insert_text(&body_permit, "重放", NSRange::new(0, 0), &|_| true),
        Err(NativeMarkdownPagedInputError::Consumed)
    ));

    let permit = fixture.body_permit();
    fixture
        .body
        .history(&permit, NativeMarkdownPagedInputHistory::Undo, &|_| true)
        .unwrap();
    assert_eq!(fixture.text(), SOURCE);
    assert_eq!(
        &*fixture.body.folds.borrow(),
        std::slice::from_ref(&fixture.fold)
    );
    assert!(
        !fixture.document.borrow().can_undo(),
        "局部typing历史不能进入canonical历史"
    );
    let permit = fixture.body_permit();
    fixture
        .body
        .history(&permit, NativeMarkdownPagedInputHistory::Redo, &|_| true)
        .unwrap();
    assert_eq!(fixture.text(), SOURCE.replacen("base", "新中\\|😀", 1));
}

#[test]
fn unchanged_cell_closes_without_body_serial_history_selection_or_fold_changes() {
    let fixture = Fixture::new();
    let before = fixture.body.state().unwrap();
    let permit = fixture.body_permit();
    let resources = fixture.session.resource_observer().unwrap();
    let result = fixture
        .cell
        .commit_cell_to_body(
            fixture.prepare(),
            &fixture.body,
            &permit,
            &|_| true,
            &|_| true,
        )
        .unwrap();
    assert!(
        matches!(result.outcome, NativeMarkdownPagedInputOutcome::Unchanged(actual) if actual == before)
    );
    assert_eq!(fixture.body.state().unwrap(), before);
    assert_eq!(fixture.text(), SOURCE);
    assert!(!fixture.document.borrow().can_undo());
    assert_eq!(
        &*fixture.body.folds.borrow(),
        std::slice::from_ref(&fixture.fold)
    );
    assert!(fixture.cell.closed.get() && resources.is_released());
}

#[test]
fn same_bytes_other_doc_body_is_rejected_without_consuming_foreign_body_permit() {
    let fixture = Fixture::new();
    let other = Fixture::new();
    fixture.replace("新值");
    let original = fixture.cell.state().unwrap();
    let permit = other.body_permit();
    assert!(matches!(
        fixture.cell.commit_cell_to_body(
            fixture.prepare(),
            &other.body,
            &permit,
            &|_| true,
            &|_| true
        ),
        Err(NativeMarkdownPagedCellCommitError::Input(
            NativeMarkdownPagedInputError::Revoked
        ))
    ));
    assert!(!permit.consumed.get());
    assert_eq!(fixture.cell.state().unwrap(), original);
    assert_eq!(fixture.text(), SOURCE);
    assert_eq!(other.text(), SOURCE);
    assert!(!fixture.cell.closed.get());
}

#[test]
fn stale_body_permit_and_busy_fold_borrow_preserve_draft_and_canonical() {
    for stale in [true, false] {
        let (fixture, stale_permit) = Fixture::with_stale_body_permit(stale, 0);
        fixture.replace("新值");
        let original = fixture.cell.state().unwrap();
        let permit = stale_permit.unwrap_or_else(|| fixture.body_permit());
        let folds = (!stale).then(|| fixture.body.folds.borrow());
        let result = fixture.cell.commit_cell_to_body(
            fixture.prepare(),
            &fixture.body,
            &permit,
            &|_| true,
            &|_| true,
        );
        match result {
            Err(NativeMarkdownPagedCellCommitError::Input(
                NativeMarkdownPagedInputError::StateChanged,
            )) if stale => {}
            Err(NativeMarkdownPagedCellCommitError::Input(
                NativeMarkdownPagedInputError::Borrowed,
            )) if !stale => {}
            _ => panic!("原Body stale/借用首因必须保留"),
        }
        drop(folds);
        assert_eq!(fixture.cell.state().unwrap(), original);
        assert_eq!(fixture.text(), SOURCE);
        assert!(!fixture.document.borrow().can_undo());
        assert!(!fixture.cell.busy.get() && !fixture.body.busy.get());
        assert!(!fixture.cell.closed.get());
        assert!(
            !fixture
                .session
                .resource_observer()
                .unwrap()
                .snapshot()
                .prepared_commit_live
        );
    }
}

#[test]
fn real_reader_blocks_commit_then_new_original_tickets_commit_after_actual_reader_drop() {
    let fixture = Fixture::new();
    fixture.replace("新值");
    let reader = fixture.session.capture_source().unwrap();
    let resources = fixture.session.resource_observer().unwrap();
    let permit = fixture.body_permit();
    assert!(matches!(
        fixture.cell.commit_cell_to_body(
            fixture.prepare(),
            &fixture.body,
            &permit,
            &|_| true,
            &|_| true
        ),
        Err(NativeMarkdownPagedCellCommitError::Commit(
            MarkdownCellDraftCommitError::Draft(MarkdownCellDraftError::ReadersBusy)
        ))
    ));
    assert_eq!(fixture.text(), SOURCE);
    assert!(!fixture.cell.closed.get() && !fixture.document.borrow().can_undo());
    assert!(!resources.snapshot().prepared_commit_live);
    drop(reader);
    let permit = fixture.body_permit();
    assert!(matches!(
        fixture
            .cell
            .commit_cell_to_body(
                fixture.prepare(),
                &fixture.body,
                &permit,
                &|_| true,
                &|_| true
            )
            .unwrap()
            .outcome,
        NativeMarkdownPagedInputOutcome::Committed(_)
    ));
    assert!(resources.is_released());
}

#[test]
fn callbacks_see_both_busy_guards_and_postcommit_revocation_does_not_replay_canonical() {
    let fixture = Fixture::new();
    fixture.replace("新值");
    let permit = fixture.body_permit();
    let active = Cell::new(true);
    let saw_busy = Cell::new(false);
    let current = |_| {
        if fixture.body.busy.get() {
            assert!(matches!(
                fixture.body.capture(BODY_EPOCH, &|_| true),
                Err(NativeMarkdownPagedInputError::Busy)
            ));
            assert!(matches!(
                fixture.cell.capture(CELL_EPOCH, &|_| true),
                Err(NativeMarkdownPagedInputError::Busy)
            ));
            saw_busy.set(true);
        }
        active.get()
    };
    let result = fixture
        .cell
        .commit_cell_to_body(
            fixture.prepare(),
            &fixture.body,
            &permit,
            &|_| true,
            &current,
        )
        .unwrap();
    let NativeMarkdownPagedInputOutcome::Committed(commit) = result.outcome else {
        panic!("实际修改缺少原Body receipt");
    };
    assert!(saw_busy.get());
    active.set(false);
    assert!(matches!(
        commit.check_current(&current),
        Err(NativeMarkdownPagedInputError::Revoked)
    ));
    assert_eq!(fixture.text(), SOURCE.replacen("base", "新值", 1));
    assert!(fixture.document.borrow().can_undo());
}

#[test]
fn original_cell_source_revocation_precedes_current_and_never_touches_canonical() {
    let fixture = Fixture::new();
    fixture.replace("新值");
    let prepared = fixture.prepare();
    let permit = fixture.body_permit();
    let resources = fixture.session.resource_observer().unwrap();
    fixture.session.close().unwrap();
    let calls = Cell::new(0);
    let current = |_| {
        calls.set(calls.get() + 1);
        false
    };
    assert!(matches!(
        fixture
            .cell
            .commit_cell_to_body(prepared, &fixture.body, &permit, &current, &current),
        Err(NativeMarkdownPagedCellCommitError::Input(
            NativeMarkdownPagedInputError::Cell(MarkdownCellDraftError::Stale)
        ))
    ));
    assert_eq!(calls.get(), 0);
    assert!(!permit.consumed.get());
    assert_eq!(fixture.text(), SOURCE);
    assert!(!fixture.document.borrow().can_undo());
    assert!(resources.is_released());
}
