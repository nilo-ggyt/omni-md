//! 只测纯fold metadata/票据与Core提交；不构造AppKit对象或代替Native验收。
use super::*;
use tempfile::{TempDir, tempdir};

struct Owner(Option<usize>);
impl Owner {
    fn enter() -> Self {
        Self(BORROWED_NATIVE_MARKDOWN_OWNER.replace(Some(77)))
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        BORROWED_NATIVE_MARKDOWN_OWNER.set(self.0);
    }
}
fn fixture() -> (TempDir, SharedMarkdownDocument, Rc<NativeMarkdownFoldState>) {
    let root = tempdir().unwrap();
    let path = root.path().join("synthetic.md");
    std::fs::write(&path, "a\nbbb\nccc\nz").unwrap();
    let document = Rc::new(RefCell::new(
        MarkdownDocumentBuffer::open(&path, 4096).unwrap(),
    ));
    let state = Rc::new(NativeMarkdownFoldState::new(
        &document,
        MarkdownPresentationMode::Source,
    ));
    state.bind_owner(77);
    state.borrow_mut().push(MarkdownFoldRegion {
        header_line: 1,
        hidden_utf16_range: 2..9,
    });
    (root, document, state)
}
fn body(document: &SharedMarkdownDocument) -> Vec<u8> {
    let mut result = Vec::new();
    document.borrow().write_to(&mut result).unwrap();
    result
}

fn source_document(source: &str) -> (TempDir, SharedMarkdownDocument) {
    let root = tempdir().unwrap();
    let path = root.path().join("fold-projection.md");
    std::fs::write(&path, source).unwrap();
    let document = Rc::new(RefCell::new(
        MarkdownDocumentBuffer::open(&path, 4096).unwrap(),
    ));
    (root, document)
}

#[test]
fn real_heading_fold_projects_closing_fence_125_to_native_paragraph_end_126() {
    let source = concat!(
        "# 中文长文与表格验收\n\n用于核对原生行号、折叠、中文查找与缩放。\n\n",
        "## 第1节：中文编辑与可见区\n\n",
        "这是一段中文长文。保留源文件坐标，滚动时按可见区绘制，编辑与查找结果应保持原来的交互。😀\n\n",
        "```rust\nlet value = 1;\n```\n\n## 第2节：中文编辑与可见区\n",
    );
    let (_root, document) = source_document(source);
    let document = document.borrow();
    let fold = omni_markdown_core::markdown_folding::markdown_fold_region(&document, 4)
        .unwrap()
        .unwrap();
    assert_eq!(fold.hidden_utf16_range, 50..125);
    assert_eq!(document.utf16_code_unit_at(125).unwrap(), 10);
    // 原 header 35..51 保留；随后整 paragraph 被 delegate 省略到 closing fence 的 LF 之后。
    assert!(!hidden_fold_union_covers(
        &(51..126),
        document.utf16_len(),
        std::slice::from_ref(&fold)
    ));
    assert!(native_hidden_fold_union_covers(
        &(51..126),
        &document,
        std::slice::from_ref(&fold)
    ));
    assert!(!native_hidden_fold_union_covers(
        &(51..127),
        &document,
        std::slice::from_ref(&fold)
    ));
    assert_eq!(fold.hidden_utf16_range, 50..125); // 投影证明不能重写 Core 保存/历史使用的原坐标。
}

#[test]
fn native_projection_extends_only_real_lf_crlf_cr_and_keeps_eof_and_following_paragraph() {
    for ending in ["\n", "\r\n", "\r"] {
        let source = format!("# heading{ending}body 😀{ending}# next{ending}");
        let (_root, document) = source_document(&source);
        let document = document.borrow();
        let fold = omni_markdown_core::markdown_folding::markdown_fold_region(&document, 0)
            .unwrap()
            .unwrap();
        let next = fold.hidden_utf16_range.end + ending.len();
        let body_start = fold.hidden_utf16_range.start + ending.len();
        assert!(native_hidden_fold_union_covers(
            &(body_start..next),
            &document,
            std::slice::from_ref(&fold)
        ));
        assert!(!native_hidden_fold_union_covers(
            &(body_start..next + 1),
            &document,
            std::slice::from_ref(&fold)
        ));
        let eof_source = format!("# heading{ending}body 😀");
        let (_eof_root, eof) = source_document(&eof_source);
        let eof = eof.borrow();
        let eof_fold = omni_markdown_core::markdown_folding::markdown_fold_region(&eof, 0)
            .unwrap()
            .unwrap();
        assert_eq!(eof_fold.hidden_utf16_range.end, eof.utf16_len());
        assert!(native_hidden_fold_union_covers(
            &(body_start..eof.utf16_len()),
            &eof,
            &[eof_fold]
        ));
    }
    // 末行内容紧邻 PS 时，后面的 LF 自成未隐藏空 paragraph；不能只因 LF 值相同就吞掉它。
    let (_root, document) = source_document("# heading\nbody\u{2029}\n# next\n");
    let document = document.borrow();
    let fold = omni_markdown_core::markdown_folding::markdown_fold_region(&document, 0)
        .unwrap()
        .unwrap();
    let gap = 10..fold.hidden_utf16_range.end + 1;
    assert!(!native_hidden_fold_union_covers(&gap, &document, &[fold]));
}

#[test]
fn native_projection_unit_budget_rejects_missing_units_and_unproved_paragraph_boundaries() {
    let calls = RefCell::new(Vec::new());
    let end = native_fold_projection_end(
        &(1..5),
        8,
        |offset| {
            calls.borrow_mut().push(offset);
            Some(if offset == 5 { 13 } else { u16::from(b'x') })
        },
        |_| Some((2, 7)),
    );
    assert_eq!(end, Some(7)); // 真实 CRLF 长度由同一物理行聚合提供，不另读第三单元。
    assert_eq!(&*calls.borrow(), &[5, 4]);
    for missing in [5, 4] {
        calls.borrow_mut().clear();
        let end = native_fold_projection_end(
            &(1..5),
            8,
            |offset| {
                calls.borrow_mut().push(offset);
                (offset != missing).then_some(if offset == 5 { 10 } else { u16::from(b'x') })
            },
            |_| Some((2, 6)),
        );
        assert_eq!(end, None);
        assert!(calls.borrow().len() <= 2); // 缺源只能拒绝扩展，不能用默认字符或 blanket end+1 补证。
    }
    for previous in [10, 13, 0x2029] {
        assert_eq!(
            native_fold_projection_end(
                &(1..5),
                8,
                |offset| Some(if offset == 5 { 10 } else { previous }),
                |_| panic!("a new empty paragraph must not be projected")
            ),
            Some(5)
        );
    }
    assert_eq!(
        native_fold_projection_end(
            &(1..5),
            8,
            |_| Some(u16::from(b'x')),
            |_| panic!("ordinary content has no ending proof")
        ),
        Some(5)
    );
    assert_eq!(
        native_fold_projection_end(
            &(1..5),
            5,
            |_| panic!("EOF must not read"),
            |_| panic!("EOF has no next line")
        ),
        Some(5)
    );
    assert_eq!(
        native_fold_projection_end(
            &(1..5),
            8,
            |offset| Some(if offset == 5 { 10 } else { u16::from(b'x') }),
            |_| Some((0, 6))
        ),
        Some(5)
    );
    assert_eq!(
        native_fold_projection_end(
            &(1..5),
            8,
            |offset| Some(if offset == 5 { 10 } else { u16::from(b'x') }),
            |_| Some((2, 7))
        ),
        None
    ); // LF 不得借 CRLF 的双单元范围。
}

#[test]
fn native_projected_gap_keeps_original_generation_revision_and_releases_all_borrows() {
    let _owner = Owner::enter();
    let (_root, document) = source_document("# heading\nbody\n# next\n");
    let fold = omni_markdown_core::markdown_folding::markdown_fold_region(&document.borrow(), 0)
        .unwrap()
        .unwrap();
    let gap = 10..fold.hidden_utf16_range.end + 1;
    let state = Rc::new(NativeMarkdownFoldState::new(
        &document,
        MarkdownPresentationMode::Source,
    ));
    state.bind_owner(77);
    state.borrow_mut().push(fold);
    let capture = state.capture_selection().unwrap();
    let generation = document.borrow().generation();
    let revision = state.revision();
    let current = || {
        assert!(document.try_borrow_mut().is_ok());
        assert!(state.ranges.try_borrow_mut().is_ok());
        capture.current()
    };
    assert!(state.hidden_range_is_covered(&gap, generation, revision, &current));
    assert!(!state.hidden_range_is_covered(&gap, generation + 1, revision, &current));
    assert!(!state.hidden_range_is_covered(&gap, generation, revision + 1, &current));
    assert!(!state.hidden_range_is_covered(&gap, generation, revision, &|| false));
    state.advance();
    assert!(!state.hidden_range_is_covered(&gap, generation, revision, &current));
    let current_revision = state.revision();
    document
        .borrow_mut()
        .replace_utf16_range(0..0, "x", MarkdownTextSelection::caret(1))
        .unwrap();
    assert!(!state.hidden_range_is_covered(&gap, generation, current_revision, &|| true)); // 即使外部票错误声称有效，同一 Doc 的真实新代次仍拒绝旧投影。
}

#[test]
fn hidden_union_accepts_unordered_nested_overlapping_and_adjacent_ranges_only_exactly() {
    let ranges = |values: &[std::ops::Range<usize>]| {
        values
            .iter()
            .cloned()
            .map(|hidden_utf16_range| MarkdownFoldRegion {
                header_line: 0,
                hidden_utf16_range,
            })
            .collect::<Vec<_>>()
    };
    let folds = ranges(&[8..12, 2..6, 4..9, 3..5]);
    assert!(hidden_fold_union_covers(&(2..12), 20, &folds));
    assert!(hidden_fold_union_covers(&(3..11), 20, &folds));
    assert!(hidden_fold_union_covers(
        &(2..9),
        20,
        &ranges(&[5..9, 2..5])
    ));
    assert!(!hidden_fold_union_covers(
        &(2..9),
        20,
        &ranges(&[2..5, 6..9])
    ));
    assert!(!hidden_fold_union_covers(&(1..12), 20, &folds));
    assert!(!hidden_fold_union_covers(&(2..13), 20, &folds));
    assert!(!hidden_fold_union_covers(
        &std::ops::Range { start: 12, end: 2 },
        20,
        &folds
    ));
    assert!(!hidden_fold_union_covers(&(2..2), 20, &folds));
    assert!(!hidden_fold_union_covers(&(2..21), 20, &folds));
    assert!(!hidden_fold_union_covers(
        &(2..6),
        20,
        &ranges(&[2..6, 19..21])
    ));
    assert!(!hidden_fold_union_covers(
        &(2..6),
        20,
        &ranges(&[2..6, std::ops::Range { start: 8, end: 7 }])
    ));
    // 端点取MAX仍只比较/减界，不计算end+1；反向或坏metadata不能借溢出形成覆盖。
    assert!(hidden_fold_union_covers(
        &(usize::MAX - 1..usize::MAX),
        usize::MAX,
        &ranges(std::slice::from_ref(&(usize::MAX - 2..usize::MAX)))
    ));
    assert!(!hidden_fold_union_covers(
        &std::ops::Range {
            start: usize::MAX,
            end: 0
        },
        usize::MAX,
        &folds
    ));
    let oversized = vec![
        MarkdownFoldRegion {
            header_line: 0,
            hidden_utf16_range: 2..6
        };
        omni_markdown_core::markdown_folding::anchor_mapping::MAX_MARKDOWN_FOLD_ANCHORS
            + 1
    ];
    assert!(!hidden_fold_union_covers(&(2..6), 20, &oversized));
}

#[test]
fn hidden_gap_proof_ends_metadata_borrows_and_keeps_original_owner_generation_revision_selection() {
    let _owner = Owner::enter();
    let (_root, document, state) = fixture();
    let capture = state.capture_selection().unwrap();
    let revision = state.revision();
    let generation = document.borrow().generation();
    let current = || {
        assert!(state.ranges.try_borrow_mut().is_ok());
        assert!(document.try_borrow_mut().is_ok()); // 同一位置可执行Native权限核，不能遗留Doc/fold借用。
        capture.current()
    };
    assert!(state.hidden_range_is_covered(&(2..9), generation, revision, &current));
    assert!(!state.hidden_range_is_covered(&(1..9), generation, revision, &current));
    assert!(!state.hidden_range_is_covered(&(2..9), generation, revision + 1, &current));
    assert!(!state.hidden_range_is_covered(&(2..9), generation + 1, revision, &current));
    assert!(!state.hidden_range_is_covered(&(2..9), generation, revision, &|| false));
    state.advance();
    assert!(!state.hidden_range_is_covered(&(2..9), generation, revision, &current));
    let capture = state.capture_selection().unwrap();
    let revision = state.revision();
    document
        .borrow_mut()
        .set_selection(MarkdownTextSelection::caret(1))
        .unwrap();
    assert!(!state.hidden_range_is_covered(&(2..9), generation, revision, &|| capture.current()));
    let capture = state.capture_selection().unwrap();
    state.bind_owner(78);
    assert!(!state.hidden_range_is_covered(&(2..9), generation, revision, &|| capture.current()));
}

#[test]
fn real_commit_and_history_publish_new_coordinates_before_any_native_notification() {
    let _owner = Owner::enter();
    let (_root, document, state) = fixture();
    let observer = state.prepare(&document.borrow(), false).unwrap();
    let prepared = document
        .borrow_mut()
        .replace_utf16_range_with_selection_observed(
            0..1,
            "a\n",
            MarkdownTextSelection::caret(0),
            MarkdownTextSelection::caret(0),
            observer,
        )
        .unwrap();
    // Core提交后还未有任何Native消息；唯一纯载荷随后一次更新fold代次与行号。
    prepared.publish(document.borrow().generation());
    assert_eq!(state.borrow()[0].hidden_utf16_range, 3..10);
    assert_eq!(state.borrow()[0].header_line, 2);
    assert_eq!(state.generation.get(), document.borrow().generation());
    let observer = state.prepare(&document.borrow(), false).unwrap();
    let (_, prepared) = document
        .borrow_mut()
        .undo_text_edit_observed(observer)
        .unwrap()
        .unwrap();
    prepared.publish(document.borrow().generation());
    assert_eq!(state.borrow()[0].hidden_utf16_range, 2..9);
    assert_eq!(state.borrow()[0].header_line, 1);
}

#[test]
fn selection_aba_mode_aba_and_seal_revoke_captured_permission() {
    let _owner = Owner::enter();
    let (_root, document, state) = fixture();
    let capture = state
        .capture_selection()
        .unwrap()
        .finish(MarkdownTextSelection::caret(0), true)
        .unwrap();
    capture.publish_head(true);
    state.advance(); // 明确第二次选择返回原位置，也不能复活最初的票据。
    assert!(!capture.current_in(&document.borrow()));
    let capture = state.capture_selection().unwrap();
    state.set_mode(MarkdownPresentationMode::InlinePreview);
    state.set_mode(MarkdownPresentationMode::Source);
    assert!(!capture.current());
    let capture = state.capture_selection().unwrap();
    state.seal();
    assert!(!capture.current());
    assert_eq!(state.borrow().capacity(), 0);
    assert!(!state.take_layout_dirty());
}

#[test]
fn stale_pure_observer_rejects_without_changing_text_selection_or_history() {
    let _owner = Owner::enter();
    let (_root, document, state) = fixture();
    let observer = state.prepare(&document.borrow(), true).unwrap();
    state.advance(); // 模拟首个Native getter后的明确fold新意图。
    let before = body(&document);
    let result = document
        .borrow_mut()
        .replace_utf16_range_with_selection_observed(
            0..1,
            "替换",
            MarkdownTextSelection::caret(0),
            MarkdownTextSelection::caret(0),
            observer,
        );
    assert!(result.is_err());
    assert_eq!(body(&document), before);
    assert_eq!(document.borrow().generation(), 0);
    assert!(!document.borrow().can_undo());
    assert_eq!(state.borrow()[0].hidden_utf16_range, 2..9);
}

#[test]
fn main_head_filter_is_strict_and_does_not_use_the_selection_anchor() {
    let _owner = Owner::enter();
    let (_root, _document, state) = fixture();
    let backward = state
        .capture_selection()
        .unwrap()
        .finish(
            MarkdownTextSelection {
                anchor_utf16: 5,
                head_utf16: 2,
            },
            true,
        )
        .unwrap();
    backward.publish_head(true); // anchor在内部，head在边界：不能展开。
    assert_eq!(state.borrow().len(), 1);
    let forward = state
        .capture_selection()
        .unwrap()
        .finish(
            MarkdownTextSelection {
                anchor_utf16: 2,
                head_utf16: 5,
            },
            true,
        )
        .unwrap();
    forward.publish_head(true);
    assert!(state.borrow().is_empty());
    assert!(state.take_layout_dirty());
}

#[test]
fn different_document_allocation_and_late_owner_never_gain_authority() {
    let _owner = Owner::enter();
    let (_root, document, state) = fixture();
    let (_other_root, other_document, _) = fixture();
    assert!(!state.matches_document(&other_document));
    let capture = state
        .capture_selection()
        .unwrap()
        .finish(MarkdownTextSelection::caret(0), true)
        .unwrap();
    assert!(capture.belongs_to(&document));
    assert!(!capture.belongs_to(&other_document));
    BORROWED_NATIVE_MARKDOWN_OWNER.set(Some(78));
    assert!(!capture.current_in(&document.borrow()));
}

#[test]
fn composition_disables_delete_scope_and_nested_standard_delete_scopes_balance() {
    let _owner = Owner::enter();
    let (_root, _document, state) = fixture();
    state.set_composition(true);
    {
        let _scope = state.begin_delete();
        assert_eq!(state.delete_depth.get(), 0);
    }
    state.set_composition(false);
    {
        let _outer = state.begin_delete();
        {
            let _inner = state.begin_delete();
            assert_eq!(state.delete_depth.get(), 2);
        }
        assert_eq!(state.delete_depth.get(), 1);
    }
    assert_eq!(state.delete_depth.get(), 0);
}

#[test]
fn unresolved_multi_selection_keeps_its_original_permission_for_the_legacy_branch() {
    let _owner = Owner::enter();
    let (_root, _document, state) = fixture();
    state.borrow_mut().clear();
    let unresolved =
        NativeMarkdownFoldError::UnresolvedSelection(state.capture_selection().unwrap());
    state.advance(); // 模拟getter局部对象析构时新选择意图到达。
    let NativeMarkdownFoldError::UnresolvedSelection(capture) = unresolved else {
        panic!("expected unresolved selection");
    };
    assert!(!capture.current()); // 无活fold也不能从新代次重领权限继续旧command。
}

#[test]
fn native_step_gate_rejects_before_work_and_after_reentrant_fold_change() {
    let _owner = Owner::enter();
    let (_root, _document, state) = fixture();
    let calls = Cell::new(0);
    let capture = state.capture_selection().unwrap();
    state.advance();
    let result = selection_native_step(&capture, || calls.set(calls.get() + 1));
    assert!(matches!(result, Err(NativeMarkdownFoldError::Stale)));
    assert_eq!(calls.get(), 0); // 已失效请求不得先执行getter再事后拒绝。

    let capture = state.capture_selection().unwrap();
    let result = selection_native_step(&capture, || {
        calls.set(calls.get() + 1);
        state.advance(); // 纯反例模拟Native getter同步交付新的fold/选择意图。
        42
    });
    assert!(matches!(result, Err(NativeMarkdownFoldError::Stale)));
    assert_eq!(calls.get(), 1); // getter仅执行一次，其旧返回值没有交付给消费方。
}

#[test]
fn commit_ticket_survives_pure_layout_consumption_but_not_a_successor_selection() {
    let _owner = Owner::enter();
    let (_root, document, state) = fixture();
    let observer = state.prepare(&document.borrow(), false).unwrap();
    let prepared = document
        .borrow_mut()
        .replace_utf16_range_with_selection_observed(
            0..1,
            "前",
            MarkdownTextSelection::caret(0),
            MarkdownTextSelection::caret(0),
            observer,
        )
        .unwrap();
    let committed = prepared.publish(document.borrow().generation());
    assert!(committed.current());
    assert!(state.take_layout_dirty()); // 派生布局消费不生成新业务意图或使本次提交票失效。
    assert!(committed.current());
    let calls = Cell::new(0);
    let result = committed.native_step(|| {
        calls.set(calls.get() + 1);
        document
            .borrow_mut()
            .set_selection(MarkdownTextSelection::caret(1))
            .unwrap();
    });
    assert!(matches!(result, Err(NativeMarkdownFoldError::Stale)));
    assert!(!committed.current()); // 即使foldrevision未被producer推进，真实后继选择也撤权。
    assert!(
        committed
            .native_step(|| calls.set(calls.get() + 1))
            .is_err()
    );
    assert_eq!(calls.get(), 1);
}

#[test]
fn unknown_generation_unfolds_metadata_and_marks_existing_native_layout_dirty() {
    let _owner = Owner::enter();
    let (_root, _document, state) = fixture();
    state.synchronize_generation(1);
    assert!(state.borrow().is_empty());
    assert!(state.take_layout_dirty()); // fail-open还须通知既有fragment，不能只换Vec。
    state.synchronize_generation(2);
    assert!(!state.take_layout_dirty()); // 已无隐藏内容时不制造无效化工作。
}

/// 只模拟同一纯发布栈；不构造Native选择或以此代替TextKit实际反向扩选验收。
fn publish_selection(
    document: &SharedMarkdownDocument,
    state: &Rc<NativeMarkdownFoldState>,
    selection: MarkdownTextSelection,
    proven: bool,
) -> NativeMarkdownCommittedSelection {
    let captured = state
        .capture_selection()
        .unwrap()
        .finish(selection, proven)
        .unwrap();
    let mut document = document.borrow_mut();
    let changed = document.selection() != selection;
    document.set_selection(selection).unwrap();
    captured.publish_head(changed)
}

#[test]
fn full_direction_publication_revokes_old_capture_and_keeps_the_same_pure_receipt() {
    let _owner = Owner::enter();
    let (_root, document, state) = fixture();
    let before = body(&document);
    let old = state.capture_selection().unwrap();
    let reverse = MarkdownTextSelection {
        anchor_utf16: 5,
        head_utf16: 2,
    };
    let committed = publish_selection(&document, &state, reverse, true);
    assert_eq!(state.known_selection(), Some(reverse));
    assert_eq!(committed.native_before_range(), NSRange::new(0, 0));
    assert!(committed.current());
    assert!(!old.current());
    assert_eq!(body(&document), before);
    assert!(!document.borrow().can_undo());
    assert_eq!(document.borrow().generation(), 0);
}

#[test]
fn unknown_nonempty_selection_cannot_publish_a_proven_head_but_caret_is_unambiguous() {
    let _owner = Owner::enter();
    let (_root, document, state) = fixture();
    let ordered = MarkdownTextSelection {
        anchor_utf16: 2,
        head_utf16: 5,
    };
    let receipt = publish_selection(&document, &state, ordered, false);
    assert!(receipt.current());
    assert_eq!(state.known_selection(), None);
    assert_eq!(state.borrow().len(), 1); // 未知head不能按排序后的末端展开fold。
    let caret = MarkdownTextSelection::caret(2);
    let receipt = publish_selection(&document, &state, caret, false);
    assert!(receipt.current());
    assert_eq!(state.known_selection(), Some(caret));
}

#[test]
fn own_install_accepts_only_exact_before_after_and_foreign_range_revokes_original_receipt() {
    let _owner = Owner::enter();
    let (_root, document, state) = fixture();
    let receipt = publish_selection(
        &document,
        &state,
        MarkdownTextSelection {
            anchor_utf16: 5,
            head_utf16: 2,
        },
        true,
    );
    let guard = receipt
        .begin_native_install(receipt.native_before_range())
        .unwrap();
    assert_eq!(
        guard.observe(NSRange::new(0, 0)),
        NativeMarkdownSelectionInstallObservation::OwnDeclared
    );
    assert_eq!(
        state.observe_native_selection(NSRange::new(2, 3)),
        Some(NativeMarkdownSelectionInstallObservation::OwnDeclared)
    );
    assert_eq!(
        guard.observe(NSRange::new(3, 1)),
        NativeMarkdownSelectionInstallObservation::Foreign
    );
    assert!(!guard.current());
    assert!(!receipt.current());
    assert_eq!(state.known_selection(), None);
    assert_eq!(state.observe_native_selection(NSRange::new(2, 3)), None);
}

#[test]
fn old_guard_drop_cannot_clear_a_successor_install_even_at_the_same_selection_serial() {
    let _owner = Owner::enter();
    let (_root, document, state) = fixture();
    let receipt = publish_selection(&document, &state, MarkdownTextSelection::caret(1), true);
    let first = receipt.begin_pre_notify_install().unwrap();
    let successor = receipt
        .begin_native_install(receipt.native_before_range())
        .unwrap();
    assert!(!first.current());
    drop(first);
    assert!(successor.current());
    assert_eq!(
        state.observe_native_selection(NSRange::new(1, 0)),
        Some(NativeMarkdownSelectionInstallObservation::OwnDeclared)
    );
    drop(successor);
    assert_eq!(state.observe_native_selection(NSRange::new(1, 0)), None);
    assert!(receipt.current()); // 结束栈内lease不撤销当前Core事实或其转换收据。
}

#[test]
fn actual_new_input_revokes_old_install_and_capture_without_erasing_core_direction() {
    let _owner = Owner::enter();
    let (_root, document, state) = fixture();
    let selection = MarkdownTextSelection {
        anchor_utf16: 5,
        head_utf16: 2,
    };
    let receipt = publish_selection(&document, &state, selection, true);
    let guard = receipt.begin_pre_notify_install().unwrap();
    let capture = state.capture_selection().unwrap();
    state.invalidate_native_install();
    assert!(!guard.current());
    assert!(!receipt.current());
    assert!(!capture.current());
    assert!(receipt.begin_pre_notify_install().is_err());
    assert_eq!(state.known_selection(), Some(selection));
    assert!(state.capture_selection().unwrap().current());
}

#[test]
fn hidden_core_direction_change_with_the_same_ordered_range_is_foreign_to_an_install() {
    let _owner = Owner::enter();
    let (_root, document, state) = fixture();
    let receipt = publish_selection(
        &document,
        &state,
        MarkdownTextSelection {
            anchor_utf16: 5,
            head_utf16: 2,
        },
        true,
    );
    let guard = receipt.begin_pre_notify_install().unwrap();
    document
        .borrow_mut()
        .set_selection(MarkdownTextSelection {
            anchor_utf16: 2,
            head_utf16: 5,
        })
        .unwrap();
    assert_eq!(
        state.observe_native_selection(NSRange::new(2, 3)),
        Some(NativeMarkdownSelectionInstallObservation::Foreign)
    );
    assert!(!guard.current());
    assert!(!receipt.current());
    assert_eq!(state.known_selection(), None);
}

#[test]
fn with_selection_keeps_original_permission_and_requires_new_head_visual_metadata() {
    let _owner = Owner::enter();
    let (_root, document, state) = fixture();
    let selection = MarkdownTextSelection::caret(1);
    let goal = NativeMarkdownVerticalGoal::new(12.0, 24.0, 600, 3).unwrap();
    let captured = state
        .capture_selection()
        .unwrap()
        .finish(selection, true)
        .unwrap()
        .with_head_side(Some(true))
        .with_vertical_goal(Some(goal));
    let committed = {
        let mut document = document.borrow_mut();
        document.set_selection(selection).unwrap();
        captured.publish_head(true)
    };
    assert!(committed.current());
    assert_eq!(state.known_head_is_upstream(), Some(true));
    assert_eq!(state.known_vertical_goal(), Some(goal));
    let captured = state
        .capture_selection()
        .unwrap()
        .finish(selection, true)
        .unwrap();
    assert_eq!(captured.head_is_upstream(), Some(true));
    assert_eq!(captured.vertical_goal(), Some(goal));
    let captured = captured
        .with_selection(MarkdownTextSelection::caret(2), true)
        .unwrap();
    let same_action = captured.clone(); // 仅复制同一票据，不能为Page后继或Find重领当前权限。
    assert_eq!(captured.head_is_upstream(), None);
    assert_eq!(captured.vertical_goal(), None);
    state.invalidate_native_install();
    assert!(!captured.current());
    assert!(!same_action.current());
    assert!(
        captured
            .with_selection(MarkdownTextSelection::caret(3), true)
            .is_err()
    );
}

#[test]
fn core_commit_installs_known_fact_and_before_range_before_native_observers() {
    let _owner = Owner::enter();
    let (_root, document, state) = fixture();
    let before = MarkdownTextSelection {
        anchor_utf16: 5,
        head_utf16: 2,
    };
    publish_selection(&document, &state, before, true);
    let old = state.capture_selection().unwrap();
    let observer = state.prepare(&document.borrow(), true).unwrap();
    let after = MarkdownTextSelection::caret(1);
    let prepared = document
        .borrow_mut()
        .replace_utf16_range_with_selection_observed(0..1, "前", before, after, observer)
        .unwrap();
    let receipt = prepared.publish(document.borrow().generation());
    assert_eq!(state.known_selection(), Some(after));
    assert_eq!(receipt.native_before_range(), NSRange::new(2, 3));
    assert!(receipt.current());
    assert!(!old.current());
    let guard = receipt.begin_pre_notify_install().unwrap();
    assert_eq!(
        guard.observe(NSRange::new(1, 0)),
        NativeMarkdownSelectionInstallObservation::OwnDeclared
    );
}

#[test]
fn invalid_ranges_and_nonfinite_goals_do_not_gain_install_or_column_authority() {
    let _owner = Owner::enter();
    let (_root, document, state) = fixture();
    let receipt = publish_selection(&document, &state, MarkdownTextSelection::caret(1), true);
    assert!(
        receipt
            .begin_native_install(NSRange::new(objc2_foundation::NSNotFound as usize, 0))
            .is_err()
    );
    assert!(
        receipt
            .begin_native_install(NSRange::new(usize::MAX - 1, 2))
            .is_err()
    );
    assert!(receipt.begin_native_install(NSRange::new(3, 1)).is_err()); // 有限且属于正文的第三范围也不能由安装入口重新声明。
    assert_eq!(state.observe_native_selection(NSRange::new(1, 0)), None);
    assert!(NativeMarkdownVerticalGoal::new(f64::NAN, 0.0, 600, 3).is_none());
    assert!(NativeMarkdownVerticalGoal::new(0.0, f64::INFINITY, 600, 3).is_none());
}

#[test]
fn observed_native_before_is_separate_from_the_original_core_ticket() {
    let _owner = Owner::enter();
    let (_root, document, state) = fixture();
    state.reset_selection();
    let captured = state
        .capture_selection()
        .unwrap()
        .finish(MarkdownTextSelection::caret(0), true)
        .unwrap()
        .with_observed_native_before(NSRange::new(2, 3))
        .unwrap()
        .with_selection(MarkdownTextSelection::caret(1), true)
        .unwrap();
    assert!(captured.current()); // Core仍是原caret0；实际Native old范围不改原权限中的Core before。
    let receipt = {
        let mut document = document.borrow_mut();
        document.set_selection(captured.selection).unwrap();
        captured.publish_head(true)
    };
    assert_eq!(receipt.native_before_range(), NSRange::new(2, 3));
    let guard = receipt.begin_pre_notify_install().unwrap();
    assert_eq!(
        guard.observe(NSRange::new(2, 3)),
        NativeMarkdownSelectionInstallObservation::OwnDeclared
    );
    assert_eq!(
        guard.observe(NSRange::new(1, 0)),
        NativeMarkdownSelectionInstallObservation::OwnDeclared
    );
    assert!(receipt.begin_native_install(NSRange::new(0, 0)).is_err());
}

#[test]
fn committed_capture_reuses_original_serial_and_never_upgrades_unknown_direction() {
    let _owner = Owner::enter();
    let (_root, document, state) = fixture();
    let reverse = MarkdownTextSelection {
        anchor_utf16: 5,
        head_utf16: 2,
    };
    let receipt = publish_selection(&document, &state, reverse, true);
    let captured = receipt.captured_selection().unwrap();
    assert_eq!(captured.selection, reverse);
    assert!(captured.current());
    state.invalidate_native_install();
    assert!(!captured.current());
    assert!(receipt.captured_selection().is_err());
    let unknown = publish_selection(&document, &state, reverse, false);
    assert!(unknown.current());
    assert!(matches!(
        unknown.captured_selection(),
        Err(NativeMarkdownFoldError::UnresolvedSelection(_))
    ));
}

#[test]
fn unknown_head_receipt_reads_only_its_exact_current_document_length_without_upgrading_direction() {
    let _owner = Owner::enter();
    let (_root, document, state) = fixture();
    let (_other_root, other_document, _) = fixture();
    let ordered = MarkdownTextSelection {
        anchor_utf16: 2,
        head_utf16: 5,
    };
    let receipt = publish_selection(&document, &state, ordered, false);
    // 范围兼容收据仍持有效原票，但它没有足以签发完整逻辑 head 的证据。
    assert!(receipt.current());
    assert_eq!(state.known_selection(), None);
    assert_eq!(receipt.current_utf16_len_for(&document), Some(11));
    assert!(matches!(
        receipt.captured_selection(),
        Err(NativeMarkdownFoldError::UnresolvedSelection(_))
    ));
    assert_eq!(state.known_selection(), None);
    // 同内容、同长度的新 allocation 不能充当原文档；失败读取也不改变原方向事实。
    assert_eq!(receipt.current_utf16_len_for(&other_document), None);
    assert!(receipt.current());
    state.invalidate_native_install();
    // 即使 Core 范围未变，后继 serial 意图仍永久撤销旧收据的精确长度读取权限。
    assert!(!receipt.current());
    assert_eq!(receipt.current_utf16_len_for(&document), None);
    assert_eq!(state.known_selection(), None);
}

#[test]
fn reset_mode_change_and_seal_revoke_install_and_release_visual_facts() {
    let _owner = Owner::enter();
    let (_root, document, state) = fixture();
    let goal = NativeMarkdownVerticalGoal::new(12.0, 24.0, 600, 3).unwrap();
    let selection = MarkdownTextSelection::caret(1);
    let captured = state
        .capture_selection()
        .unwrap()
        .finish(selection, true)
        .unwrap()
        .with_head_side(Some(true))
        .with_vertical_goal(Some(goal));
    let receipt = {
        let mut document = document.borrow_mut();
        document.set_selection(selection).unwrap();
        captured.publish_head(true)
    };
    let guard = receipt.begin_pre_notify_install().unwrap();
    state.set_mode(MarkdownPresentationMode::InlinePreview);
    assert!(!guard.current());
    assert!(!receipt.current());
    assert_eq!(state.known_selection(), Some(selection));
    assert_eq!(state.known_vertical_goal(), None);
    assert_eq!(state.known_head_is_upstream(), None);
    state.reset_selection();
    assert_eq!(state.known_selection(), None);
    state.seal();
    assert_eq!(state.known_selection(), None);
    assert_eq!(state.observe_native_selection(NSRange::new(1, 0)), None);
}
