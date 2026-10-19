//! 真Cell准入消费原Body票；不创建NSApplication或用Noop observer替代折叠发布。

use super::*;
use omni_markdown_core::markdown_document_buffer::{
    MarkdownTableWindowStep, MarkdownTableWindowTarget,
};

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
        Self::with_stale_body_permit(false).0
    }

    /// stale负控必须在Cell准入之前制造；草稿存活期禁止canonical选区修改正是被测事务门。
    fn with_stale_body_permit(stale: bool) -> (Self, Option<NativeMarkdownPagedInputPermit>) {
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
        // 先捕获原Body票，再用另一真实Body票发布选区。此时尚未begin草稿，
        // canonical允许这次操作；后续旧票会因真实serial变化失效，而非测试改私有字段。
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
        let session = NativeMarkdownPagedCellSession::begin(
            Rc::downgrade(&document),
            Rc::new(verified),
            1,
            0,
        )
        .unwrap();
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
            .prepare_cell_commit(&permit, &|epoch| epoch == CELL_EPOCH)
            .unwrap()
    }
    fn body_permit(&self) -> NativeMarkdownPagedInputPermit {
        self.body
            .capture(BODY_EPOCH, &|epoch| epoch == BODY_EPOCH)
            .unwrap()
    }
    fn text(&self) -> String {
        let original = self.document.borrow();
        assert!(original.utf16_len() < 256); // 仅有限oracle，桥接生产不捕获canonical全文。
        let mut units = [0; 256];
        let length = original.utf16_len();
        original
            .copy_utf16_code_units(0, &mut units[..length])
            .unwrap();
        String::from_utf16(&units[..length]).unwrap()
    }
}

#[test]
fn real_cell_commit_updates_existing_body_folds_global_selection_and_one_undo_redo() {
    let fixture = Fixture::new();
    let before = fixture.body.state().unwrap();
    fixture.replace("新中|😀");
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
    let NativeMarkdownPagedInputOutcome::Committed(commit) = outcome else {
        panic!("真实修改必须返回Body commit");
    };
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
        matches!(result, NativeMarkdownPagedInputOutcome::Unchanged(actual) if actual == before)
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
        let (fixture, stale_permit) = Fixture::with_stale_body_permit(stale);
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
            .unwrap(),
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
    let NativeMarkdownPagedInputOutcome::Committed(commit) = result else {
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
