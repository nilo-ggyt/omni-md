//! 纯 owner 回归只验证原票和槽身份；实际 superclass 通知由 Native9 单独执行。

use super::*;

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

fn fixture() -> (
    tempfile::TempDir,
    SharedMarkdownDocument,
    Rc<NativeMarkdownFoldState>,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("synthetic.md");
    std::fs::write(&path, "abcdef\n").unwrap();
    let document = Rc::new(RefCell::new(
        MarkdownDocumentBuffer::open(&path, 4096).unwrap(),
    ));
    let state = Rc::new(NativeMarkdownFoldState::new(
        &document,
        MarkdownPresentationMode::Source,
    ));
    state.bind_owner(77);
    (directory, document, state)
}

fn publish(
    document: &SharedMarkdownDocument,
    state: &Rc<NativeMarkdownFoldState>,
    at: usize,
) -> Rc<NativeMarkdownCommittedSelection> {
    let captured = state
        .capture_selection()
        .unwrap()
        .finish(MarkdownTextSelection::caret(at), true)
        .unwrap();
    let mut document = document.borrow_mut();
    let changed = document.selection() != captured.selection;
    document.set_selection(captured.selection).unwrap();
    Rc::new(captured.publish_head(changed))
}

fn owner(
    receipt: Rc<NativeMarkdownCommittedSelection>,
    before: NSRange,
    eof: usize,
) -> Rc<NativeMarkdownPendingSelectionOwner> {
    let selection = receipt.selection;
    let after = NSRange::new(
        selection.anchor_utf16.min(selection.head_utf16),
        selection.anchor_utf16.abs_diff(selection.head_utf16),
    );
    Rc::new(NativeMarkdownPendingSelectionOwner {
        id: receipt.selection_serial(),
        committed: receipt,
        phase: Cell::new(NativeMarkdownPendingSelectionPhase::Published),
        source: "pure_test",
        sampling: Cell::new(false),
        revoked: Cell::new(false),
        #[cfg(feature = "native-markdown-runtime-probe")]
        reads: Rc::new(Cell::new(0)),
        #[cfg(feature = "native-markdown-runtime-probe")]
        fault: None,
        ranges: [
            Some(before),
            Some(after),
            None,
            None,
            None,
            None,
            Some(NSRange::new(eof, 0)),
            None,
            None,
        ],
    })
}

#[test]
fn foreign_b_then_declared_a_never_revives_original_receipt() {
    let _owner = Owner::enter();
    let (_directory, document, state) = fixture();
    let receipt = publish(&document, &state, 1);
    let pending = owner(Rc::clone(&receipt), NSRange::new(0, 0), 7);
    assert!(pending.observe(true, 1, NSRange::new(1, 0)));
    assert!(!pending.observe(true, 1, NSRange::new(3, 0)));
    assert_eq!(state.known_selection(), None);
    assert!(!pending.observe(true, 1, NSRange::new(1, 0)));
    assert!(!receipt.current());
}

#[test]
fn declared_eof_handoff_preserves_original_full_direction_proof() {
    let _owner = Owner::enter();
    let (_directory, document, state) = fixture();
    let selection = MarkdownTextSelection {
        anchor_utf16: 5,
        head_utf16: 2,
    };
    let captured = state
        .capture_selection()
        .unwrap()
        .finish(selection, true)
        .unwrap();
    document.borrow_mut().set_selection(selection).unwrap();
    let receipt = Rc::new(captured.publish_head(true));
    let pending = owner(Rc::clone(&receipt), NSRange::new(0, 0), 7);
    let slot = Rc::new(RefCell::new(Some(Rc::clone(&pending))));
    let guard = NativeMarkdownPendingSelectionGuard {
        slot: Rc::clone(&slot),
        owner: Rc::clone(&pending),
    };
    // 本次纯提交声明的 EOF 只是一种合法通知暂态，不是替换原完整 anchor/head 的新选择。
    assert_eq!(pending.source_hits(NSRange::new(7, 0)), 64);
    assert!(pending.observe(true, 1, NSRange::new(7, 0)));
    assert!(pending.owns_slot(&slot));
    assert_eq!(state.known_selection(), Some(selection));
    assert_eq!(receipt.captured_selection().unwrap().selection, selection);
    // 安装器应继续消费同一原收据；只有实际 after 才能完成，不能把 EOF 当作目标。
    assert!(pending.observe(true, 1, NSRange::new(2, 3)));
    assert!(receipt.current());
    drop(guard);
    assert!(slot.borrow().is_none());
}

#[test]
fn old_guard_cannot_clear_successor_slot_even_when_labels_match() {
    let _owner = Owner::enter();
    let (_directory, document, state) = fixture();
    let receipt = publish(&document, &state, 1);
    let old = owner(Rc::clone(&receipt), NSRange::new(0, 0), 7);
    let successor = owner(receipt, NSRange::new(0, 0), 7); // 标签故意相同，仍是不同的 owner Rc。
    let slot = Rc::new(RefCell::new(Some(Rc::clone(&old))));
    let guard = NativeMarkdownPendingSelectionGuard {
        slot: Rc::clone(&slot),
        owner: old,
    };
    slot.replace(Some(Rc::clone(&successor)));
    drop(guard);
    assert!(
        slot.borrow()
            .as_ref()
            .is_some_and(|actual| Rc::ptr_eq(actual, &successor))
    );
    drop(NativeMarkdownPendingSelectionGuard {
        slot: Rc::clone(&slot),
        owner: successor,
    });
    assert!(slot.borrow().is_none());
}

#[test]
fn rejecting_old_owner_preserves_real_successor_known_selection() {
    let _owner = Owner::enter();
    let (_directory, document, state) = fixture();
    let old = owner(publish(&document, &state, 1), NSRange::new(0, 0), 7);
    let successor = publish(&document, &state, 4);
    old.reject();
    assert!(!old.current());
    assert!(successor.current());
    assert_eq!(
        state.known_selection(),
        Some(MarkdownTextSelection::caret(4))
    );
}

#[test]
fn multi_unknown_and_reader_reentry_are_sticky_without_recursive_work() {
    let _owner = Owner::enter();
    let (_directory, document, state) = fixture();
    // 各反例从新合法意图开始；不能通过清 revoked 或恢复原 raw 范围重用旧 owner。
    for (present, count) in [(true, 2), (false, 0), (true, 0)] {
        let pending = owner(publish(&document, &state, 1), NSRange::new(0, 0), 7);
        assert!(!pending.observe(present, count, NSRange::new(1, 0)));
        assert!(pending.begin_sample().is_none());
        assert_eq!(state.known_selection(), None);
    }
    let pending = owner(publish(&document, &state, 1), NSRange::new(0, 0), 7);
    let first = pending.begin_sample().unwrap();
    assert!(pending.begin_sample().is_none());
    drop(first);
    assert!(pending.begin_sample().is_none());
    assert_eq!(state.known_selection(), None);
}

#[test]
fn original_native_before_survives_shrink_beyond_new_eof_without_new_source_reads() {
    let _owner = Owner::enter();
    let (_directory, document, state) = fixture();
    let captured = state
        .capture_selection()
        .unwrap()
        .finish(MarkdownTextSelection::caret(0), false)
        .unwrap()
        .with_observed_native_before(NSRange::new(7, 0))
        .unwrap();
    let observer = state
        .prepare(&document.borrow(), true)
        .unwrap()
        .with_native_before(&captured)
        .unwrap();
    let prepared = document
        .borrow_mut()
        .replace_utf16_range_with_selection_observed(
            1..7,
            "",
            MarkdownTextSelection::caret(0),
            MarkdownTextSelection::caret(1),
            observer,
        )
        .unwrap();
    let receipt = Rc::new(prepared.publish(document.borrow().generation()));
    assert_eq!(document.borrow().utf16_len(), 1);
    assert_eq!(receipt.native_before_range(), NSRange::new(7, 0));
    assert_eq!(receipt.native_before_utf16_len(), 7);
    let following = receipt.captured_selection().unwrap();
    {
        let document = document.borrow();
        // 原 before 长度继续为 7，但后继 capture 必须认证当前长度 1，不能把旧 root 长度贴到新票上。
        assert!(following.current_in(&document));
        assert!(
            state
                .prepare(&document, true)
                .unwrap()
                .with_native_before(&following)
                .is_ok()
        );
    }
    let pending = owner(receipt, NSRange::new(7, 0), 1);
    assert!(pending.observe(true, 1, NSRange::new(7, 0)));
    assert_eq!(pending.source_hits(NSRange::new(1, 0)), 2 | 64);
    assert!(pending.observe(true, 1, NSRange::new(1, 0)));
}

fn replacement_sources(
    replacement: NSRange,
    caret: NSRange,
    is_input_replacement: bool,
) -> NativeMarkdownPendingSelectionSources {
    NativeMarkdownPendingSelectionSources {
        source: "pure_eof_test",
        replacement: Some(replacement),
        caret: Some(caret),
        marked_before: None,
        marked_after: None,
        is_input_replacement,
    }
}

#[test]
fn measured_growing_and_shrinking_input_derive_only_original_eof_sources() {
    // 两组数字来自真实输入：10687 中的 9..11 换成四单元，再把新 9..13 换成两单元。
    // 期待值直接固定为原 root EOF 与删除阶段 EOF，不用测试里的另一套算法重新算同一结果。
    for (before, after, range, caret, expected) in [
        (10687, 10689, NSRange::new(9, 2), NSRange::new(13, 0), 10687),
        (10689, 10687, NSRange::new(9, 4), NSRange::new(11, 0), 10689),
    ] {
        let input = replacement_sources(range, caret, true);
        assert_eq!(
            native_markdown_pending_selection_eof_sources(before, after, input),
            Some((NSRange::new(expected, 0), Some(NSRange::new(10685, 0)))),
        );
        // 相同 canonical 编辑参数没有真实 input primitive 权限，不能取得 superclass 删除阶段来源。
        let canonical = replacement_sources(range, caret, false);
        assert_eq!(
            native_markdown_pending_selection_eof_sources(before, after, canonical),
            Some((NSRange::new(expected, 0), None)),
        );
    }
}

#[test]
fn malformed_replacement_caret_and_post_length_cannot_derive_eof_permissions() {
    for (range, caret, post) in [
        (NSRange::new(10686, 2), NSRange::new(10690, 0), 10689),
        (
            NSRange::new(usize::MAX, 1),
            NSRange::new(usize::MAX, 0),
            10689,
        ),
        (NSRange::new(9, 2), NSRange::new(8, 0), 10689),
        (NSRange::new(9, 2), NSRange::new(13, 1), 10689),
        (NSRange::new(9, 2), NSRange::new(13, 0), 10690),
    ] {
        // 越界/溢出替换、无法表示插入末端的 caret、以及不一致候选长度，都不能靠夹取补成合法来源。
        assert!(
            native_markdown_pending_selection_eof_sources(
                10687,
                post,
                replacement_sources(range, caret, true),
            )
            .is_none()
        );
    }
}

fn owner_with_phase_eofs(
    receipt: Rc<NativeMarkdownCommittedSelection>,
    phase: NativeMarkdownPendingSelectionPhase,
) -> Rc<NativeMarkdownPendingSelectionOwner> {
    let mut pending = owner(receipt, NSRange::new(0, 0), 7);
    let original = Rc::get_mut(&mut pending).unwrap();
    original.phase.set(phase);
    // 11 与 9 故意不与旧七个来源 0、1、7 重合；本测试不能因为别的合法 hit 掩盖阶段限制。
    original.ranges[7] = Some(NSRange::new(11, 0));
    original.ranges[8] = Some(NSRange::new(9, 0));
    pending
}

#[test]
fn declared_phase_eofs_preserve_the_original_known_selection() {
    let _owner = Owner::enter();
    let (_directory, document, state) = fixture();
    for (phase, raw, expected_hit) in [
        (
            NativeMarkdownPendingSelectionPhase::EndEditing,
            NSRange::new(11, 0),
            128,
        ),
        (
            NativeMarkdownPendingSelectionPhase::SuperclassTail,
            NSRange::new(11, 0),
            128,
        ),
        (
            NativeMarkdownPendingSelectionPhase::SuperclassTail,
            NSRange::new(9, 0),
            256,
        ),
    ] {
        let receipt = publish(&document, &state, 1);
        let pending = owner_with_phase_eofs(Rc::clone(&receipt), phase);
        assert_eq!(pending.source_hits(raw), expected_hit);
        assert!(pending.observe(true, 1, raw));
        assert_eq!(
            state.known_selection(),
            Some(MarkdownTextSelection::caret(1))
        );
        assert_eq!(
            receipt.captured_selection().unwrap().selection,
            MarkdownTextSelection::caret(1)
        );
        assert!(receipt.current()); // 合法暂态只交接原通知，不能把 Core 选择改成该 EOF。
    }
}

#[test]
fn wrong_phase_eof_then_valid_phase_and_after_never_revive_the_original_receipt() {
    let _owner = Owner::enter();
    let (_directory, document, state) = fixture();
    for (phase, raw) in [
        (
            NativeMarkdownPendingSelectionPhase::Published,
            NSRange::new(11, 0),
        ),
        (
            NativeMarkdownPendingSelectionPhase::EditedNotification,
            NSRange::new(11, 0),
        ),
        (
            NativeMarkdownPendingSelectionPhase::Attributes,
            NSRange::new(11, 0),
        ),
        (
            NativeMarkdownPendingSelectionPhase::Published,
            NSRange::new(9, 0),
        ),
        (
            NativeMarkdownPendingSelectionPhase::EditedNotification,
            NSRange::new(9, 0),
        ),
        (
            NativeMarkdownPendingSelectionPhase::Attributes,
            NSRange::new(9, 0),
        ),
        (
            NativeMarkdownPendingSelectionPhase::EndEditing,
            NSRange::new(9, 0),
        ),
    ] {
        let receipt = publish(&document, &state, 1);
        let pending = owner_with_phase_eofs(Rc::clone(&receipt), phase);
        assert_eq!(pending.source_hits(raw), 0);
        assert!(!pending.observe(true, 1, raw));
        assert_eq!(state.known_selection(), None);
        pending
            .phase
            .set(NativeMarkdownPendingSelectionPhase::SuperclassTail);
        // 改成合法阶段虽有同一个固定来源 hit，也不能撤销错误阶段已留下的 revoked tombstone。
        assert_ne!(pending.source_hits(raw), 0);
        assert!(!pending.observe(true, 1, raw));
        assert!(!pending.observe(true, 1, NSRange::new(1, 0)));
        assert!(!receipt.current());
        assert_eq!(state.known_selection(), None);
    }
}
