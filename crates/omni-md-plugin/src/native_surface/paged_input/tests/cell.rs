//! 生产Cell target与同Doc真实认证表格；不借NSTextField或第二Doc保存原草稿。

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
    let permit = input.capture(1, &|_| true).unwrap();
    let length = input.state().unwrap().utf16_len;
    assert!(length <= 256); // 小oracle允许物化，2MiB用固定片逐段核验，不能测试后门变产品读取协议。
    let mut units = [0; 256];
    input
        .copy_utf16_code_units(&permit, 0, &mut units[..length], &|_| true)
        .unwrap();
    String::from_utf16(&units[..length]).unwrap()
}

fn select(input: &Rc<NativeMarkdownPagedInput>, selection: MarkdownTextSelection) {
    let permit = input.capture(1, &|_| true).unwrap();
    input.set_selection(&permit, selection, &|_| true).unwrap();
}

fn insert(input: &Rc<NativeMarkdownPagedInput>, value: &str) {
    let permit = input.capture(1, &|_| true).unwrap();
    input
        .insert_text(&permit, value, unspecified(), &|_| true)
        .unwrap();
}

#[test]
fn actual_cell_ime_and_typing_groups_do_not_publish_canonical_and_match_measured_selections() {
    for ime in [true, false] {
        let (_file, document, session, input) = cell_fixture();
        // 工具栏按这一个真实局部栈启用；原canonical是否有历史不能替代草稿Undo可用性。
        assert_eq!(
            input.cell_history_facts().unwrap(),
            Some(MarkdownCellDraftHistoryFacts {
                can_undo: false,
                can_redo: false,
            })
        );
        if ime {
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
        // 一组连续输入只撤一次；局部栈耗尽后菜单禁用，不穿透为正文Undo。
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
/// 只读格和活动草稿即使属于同一 Doc，也不能把局部 UTF16 冒充正文或借用彼此输入权限。
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
    drop(draft); // 原草稿 reader 真正归还，原只读格不参加这个名额或原会话的取消门。
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
        // begin的原票核验在busy之前；进入真实转换的guard后撤原会话，借用仍必须已全部归还。
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

struct Observe;
impl MarkdownTextChangeObserver for Observe {
    type Error = std::convert::Infallible;
    type Output = ();
    fn changed(&mut self, _: MarkdownTextChange) -> std::result::Result<(), Self::Error> {
        Ok(())
    }
    fn finish(
        self,
        _: MarkdownPreparedText<'_>,
        _: MarkdownTextSelection,
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }
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
    let permit = input.capture(1, &|_| true).unwrap();
    let prepared = input
        .prepare_cell_commit(&permit, None, None, &|_| true)
        .unwrap();
    assert!(observer.snapshot().prepared_commit_live);
    assert!(matches!(
        input.commit_cell_observed(prepared, Observe, &|_| true),
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
    assert!(
        input
            .commit_cell_observed(prepared, Observe, &|_| true)
            .unwrap()
            .edit
            .is_some()
    );
    assert!(observer.is_released());
    assert!(document.borrow().can_undo());
    document.borrow_mut().undo().unwrap();
    assert_eq!(text(&document), SOURCE);
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
    let added = "x".repeat(2 * 1024 * 1024); // 仅本次真实新增输入；Doc保存增量段，不保这个测试String。
    insert(&input, &added);
    drop(added);
    let first = session.capture_source().unwrap();
    let second = session.capture_source().unwrap();
    let state = input.state().unwrap();
    assert_eq!(state.utf16_len, 2 * 1024 * 1024);
    let observer = session.resource_observer().unwrap();
    let permit = input.capture(1, &|_| true).unwrap();
    let prepared = input
        .prepare_cell_commit(&permit, None, None, &|_| true)
        .unwrap();
    // 大结果允许流式准备；实际Scene仍持读取时必须拒绝发布，并归还这份准入结果。
    assert!(observer.snapshot().prepared_commit_live);
    assert!(matches!(
        input.commit_cell_observed(prepared, Observe, &|_| true),
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
    // 原读取者真正结束后重新准备，仍由同一草稿和唯一canonical事务提交。
    let prepared = input
        .prepare_cell_commit(&permit, None, None, &|_| true)
        .unwrap();
    drop(first);
    drop(second);
    assert!(
        input
            .commit_cell_observed(prepared, Observe, &|_| true)
            .unwrap()
            .edit
            .is_some()
    );
    assert!(observer.is_released());
    let start = SOURCE.find("base").unwrap();
    let length = state.utf16_len;
    for replay in [false, true] {
        // 逐4Ki UTF16核整个新增范围，首尾固定原文单独核对；不物化两MiB结果。
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
            // 一次Undo完整恢复原文且无额外空历史；一次Redo恢复同一个大文件结果。
            document.borrow_mut().undo().unwrap();
            assert_eq!(text(&document), SOURCE);
            assert!(!document.borrow().can_undo());
            assert!(document.borrow().can_redo());
            document.borrow_mut().redo().unwrap();
        }
    }
    input.close().unwrap();
    session.close().unwrap();
}

#[test]
fn input_and_old_domain_are_weak_and_do_not_keep_cell_session_or_document_alive() {
    let (_file, document, session, input) = cell_fixture();
    let source = session.capture_source().unwrap();
    let domain = input.target_domain().unwrap();
    let observer = session.resource_observer().unwrap();
    let session_weak = Rc::downgrade(&session);
    let doc_weak = Rc::downgrade(&document);
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
