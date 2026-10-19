//! 生产Input直接消费真实Core组合组；这里只验事务和同步状态，不冒充Native键盘或候选窗验收。

use super::*;

/// 直接消费生产Input的原历史收据，不用新replacement模拟Undo/Redo。
fn history(
    owner: &Rc<NativeMarkdownPagedInput>,
    direction: NativeMarkdownPagedInputHistory,
) -> NativeMarkdownPagedInputCommit {
    let permit = owner.capture(1, &|_| true).unwrap();
    committed(owner.history(&permit, direction, &|_| true).unwrap())
}

#[test]
/// 合法显式UTF16子范围保留两侧旧候选，非法代理项切口不能断组或污染选择。
fn explicit_non_bmp_subrange_updates_merge_the_whole_composition_history() {
    let (_file, document) = fixture("甲乙", 4096);
    document
        .borrow_mut()
        .set_selection(MarkdownTextSelection::caret(1))
        .unwrap();
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let permit = owner.capture(1, &|_| true).unwrap();
    owner
        .set_marked_text(
            &permit,
            "中🙂文",
            NSRange::new(1, 2),
            unspecified(),
            &|_| true,
        )
        .unwrap();
    let permit = owner.capture(1, &|_| true).unwrap();
    let partial = owner
        .set_marked_text(
            &permit,
            "汉😀",
            NSRange::new(1, 2),
            NSRange::new(2, 2),
            &|_| true,
        )
        .unwrap();
    assert_eq!(text(&document), "甲中汉😀文乙");
    assert_eq!(partial.state.marked, Some(NSRange::new(2, 3)));
    assert_eq!(
        partial.state.selection,
        MarkdownTextSelection {
            anchor_utf16: 3,
            head_utf16: 5
        }
    );
    let stable = owner.state().unwrap();
    let permit = owner.capture(1, &|_| true).unwrap();
    assert!(
        owner
            .set_marked_text(
                &permit,
                "不能切开代理项",
                NSRange::new(0, 0),
                NSRange::new(4, 1),
                &|_| true,
            )
            .is_err()
    );
    assert_eq!(owner.state().unwrap(), stable);
    assert_eq!(text(&document), "甲中汉😀文乙");
    let permit = owner.capture(1, &|_| true).unwrap();
    let confirmed = owner
        .insert_text(&permit, "好🙂", unspecified(), &|_| true)
        .unwrap();
    assert_eq!(confirmed.state.marked, None);
    assert_eq!(text(&document), "甲中好🙂文乙");
    let undo = history(&owner, NativeMarkdownPagedInputHistory::Undo);
    assert_eq!(text(&document), "甲乙");
    assert_eq!(undo.state.selection, MarkdownTextSelection::caret(1));
    let permit = owner.capture(1, &|_| true).unwrap();
    assert!(matches!(
        owner
            .history(&permit, NativeMarkdownPagedInputHistory::Undo, &|_| true)
            .unwrap(),
        NativeMarkdownPagedInputOutcome::Unchanged(_)
    ));
    let redo = history(&owner, NativeMarkdownPagedInputHistory::Redo);
    assert_eq!(text(&document), "甲中好🙂文乙");
    assert_eq!(redo.state.selection, confirmed.state.selection);
}

#[test]
/// 跨出当前marked即独立成项，逐次Undo/Redo必须恢复被替换的外部正文。
fn explicit_replacement_outside_or_across_marked_starts_an_independent_history() {
    for (replacement, expected) in [
        (NSRange::new(4, 1), "甲中🙂新丙"),
        (NSRange::new(1, 4), "甲新丙"),
    ] {
        let (_file, document) = fixture("甲乙丙", 4096);
        document
            .borrow_mut()
            .set_selection(MarkdownTextSelection::caret(1))
            .unwrap();
        let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
        let permit = owner.capture(1, &|_| true).unwrap();
        owner
            .set_marked_text(
                &permit,
                "中🙂",
                NSRange::new(3, 0),
                unspecified(),
                &|_| true,
            )
            .unwrap();
        let permit = owner.capture(1, &|_| true).unwrap();
        owner
            .set_marked_text(&permit, "新", NSRange::new(1, 0), replacement, &|_| true)
            .unwrap();
        assert_eq!(text(&document), expected);
        let permit = owner.capture(1, &|_| true).unwrap();
        committed(owner.unmark(&permit, &|_| true).unwrap());
        history(&owner, NativeMarkdownPagedInputHistory::Undo);
        assert_eq!(text(&document), "甲中🙂乙丙");
        history(&owner, NativeMarkdownPagedInputHistory::Undo);
        assert_eq!(text(&document), "甲乙丙");
        history(&owner, NativeMarkdownPagedInputHistory::Redo);
        history(&owner, NativeMarkdownPagedInputHistory::Redo);
        assert_eq!(text(&document), expected);
    } // 当前marked以外的正文有自己的真实Undo，不能被前一组合的净日志吞并。
}

#[test]
/// unmark无假字符历史；后来的选择移动不能改写原组合的Redo目标或原选择方向。
fn unmark_keeps_one_group_and_redo_restores_the_actual_final_selection_after_a_later_move() {
    let (_file, document) = fixture("甲🙂乙", 4096);
    let before = MarkdownTextSelection {
        anchor_utf16: 3,
        head_utf16: 1,
    };
    document.borrow_mut().set_selection(before).unwrap();
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    for text in ["中😀", "汉🙂文"] {
        let permit = owner.capture(1, &|_| true).unwrap();
        owner
            .set_marked_text(&permit, text, NSRange::new(1, 2), unspecified(), &|_| true)
            .unwrap();
    }
    let final_state = owner.state().unwrap();
    let generation = final_state.generation;
    let permit = owner.capture(1, &|_| true).unwrap();
    let unmarked = committed(owner.unmark(&permit, &|_| true).unwrap());
    assert_eq!(unmarked.state.generation, generation);
    assert_eq!(unmarked.state.selection, final_state.selection);
    assert_eq!(unmarked.state.marked, None);
    let permit = owner.capture(1, &|_| true).unwrap();
    committed(
        owner
            .set_selection(&permit, MarkdownTextSelection::caret(0), &|_| true)
            .unwrap(),
    );
    assert_eq!(
        history(&owner, NativeMarkdownPagedInputHistory::Undo)
            .state
            .selection,
        before
    );
    assert_eq!(text(&document), "甲🙂乙");
    let redo = history(&owner, NativeMarkdownPagedInputHistory::Redo);
    assert_eq!(text(&document), "甲汉🙂文乙");
    assert_eq!(redo.state.selection, final_state.selection);
    assert_eq!(redo.state.marked, None);
}

#[test]
/// 组合不能吞掉相邻普通输入；三笔真实用户事务按原顺序独立恢复。
fn ordinary_typing_before_and_after_the_composition_keeps_three_separate_undo_steps() {
    let (_file, document) = fixture("甲", 4096);
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let permit = owner.capture(1, &|_| true).unwrap();
    owner
        .insert_text(&permit, "A", unspecified(), &|_| true)
        .unwrap();
    for text in ["中", "🙂"] {
        let permit = owner.capture(1, &|_| true).unwrap();
        owner
            .set_marked_text(
                &permit,
                text,
                NSRange::new(text.encode_utf16().count(), 0),
                unspecified(),
                &|_| true,
            )
            .unwrap();
    }
    let permit = owner.capture(1, &|_| true).unwrap();
    owner
        .insert_text(&permit, "文", unspecified(), &|_| true)
        .unwrap();
    let permit = owner.capture(1, &|_| true).unwrap();
    owner
        .insert_text(&permit, "B", unspecified(), &|_| true)
        .unwrap();
    assert_eq!(text(&document), "A文B甲");
    for expected in ["A文甲", "A甲", "甲"] {
        history(&owner, NativeMarkdownPagedInputHistory::Undo);
        assert_eq!(text(&document), expected);
    }
    for expected in ["A甲", "A文甲", "A文B甲"] {
        history(&owner, NativeMarkdownPagedInputHistory::Redo);
        assert_eq!(text(&document), expected);
    }
}

#[test]
/// 容量拒绝保留原marked和分组，后续成功更新及确认仍只撤销一次。
fn failed_marked_candidate_keeps_the_group_available_for_a_later_real_confirmation() {
    let (_file, document) = fixture("首", 16);
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let permit = owner.capture(1, &|_| true).unwrap();
    owner
        .set_marked_text(
            &permit,
            "中🙂",
            NSRange::new(3, 0),
            unspecified(),
            &|_| true,
        )
        .unwrap();
    let stable = owner.state().unwrap();
    let permit = owner.capture(1, &|_| true).unwrap();
    assert!(
        owner
            .set_marked_text(
                &permit,
                "无法容纳的新候选",
                NSRange::new(0, 0),
                unspecified(),
                &|_| true
            )
            .is_err()
    );
    assert_eq!(owner.state().unwrap(), stable);
    let permit = owner.capture(1, &|_| true).unwrap();
    owner
        .set_marked_text(&permit, "文", NSRange::new(1, 0), unspecified(), &|_| true)
        .unwrap();
    let permit = owner.capture(1, &|_| true).unwrap();
    owner
        .insert_text(&permit, "字", unspecified(), &|_| true)
        .unwrap();
    assert_eq!(text(&document), "字首");
    history(&owner, NativeMarkdownPagedInputHistory::Undo);
    assert_eq!(text(&document), "首");
    let permit = owner.capture(1, &|_| true).unwrap();
    assert!(matches!(
        owner
            .history(&permit, NativeMarkdownPagedInputHistory::Undo, &|_| true)
            .unwrap(),
        NativeMarkdownPagedInputOutcome::Unchanged(_)
    ));
}

#[test]
/// 关闭只封本地输入；后继client的首个marked必须Start，不能借同Doc旧范围合并上个组合。
fn closing_input_keeps_the_document_but_a_new_client_starts_an_independent_composition() {
    let (_file, document) = fixture("甲乙", 4096);
    document
        .borrow_mut()
        .set_selection(MarkdownTextSelection::caret(1))
        .unwrap();
    let old = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let permit = old.capture(1, &|_| true).unwrap();
    let first = old
        .set_marked_text(
            &permit,
            "中🙂",
            NSRange::new(0, 3),
            unspecified(),
            &|_| true,
        )
        .unwrap();
    assert_eq!(text(&document), "甲中🙂乙");
    old.close().unwrap();
    assert!(matches!(
        old.state(),
        Err(NativeMarkdownPagedInputError::Closed)
    ));
    drop(old);
    let current = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let fresh = current.state().unwrap();
    assert_eq!(fresh.marked, None);
    assert_eq!(fresh.generation, first.state.generation);
    assert_eq!(fresh.primary_range, NSRange::new(1, 3));
    let permit = current.capture(1, &|_| true).unwrap();
    current
        .set_marked_text(
            &permit,
            "新😀",
            NSRange::new(3, 0),
            unspecified(),
            &|_| true,
        )
        .unwrap();
    let permit = current.capture(1, &|_| true).unwrap();
    let confirmed = current
        .insert_text(&permit, "确认", unspecified(), &|_| true)
        .unwrap();
    assert_eq!(text(&document), "甲确认乙");
    let undo = history(&current, NativeMarkdownPagedInputHistory::Undo);
    assert_eq!(text(&document), "甲中🙂乙");
    assert_eq!(undo.state.selection, first.state.selection);
    history(&current, NativeMarkdownPagedInputHistory::Undo);
    assert_eq!(text(&document), "甲乙");
    history(&current, NativeMarkdownPagedInputHistory::Redo);
    assert_eq!(text(&document), "甲中🙂乙");
    let redo = history(&current, NativeMarkdownPagedInputHistory::Redo);
    assert_eq!(text(&document), "甲确认乙");
    assert_eq!(redo.state.selection, confirmed.state.selection);
}

#[cfg(feature = "native-source-reclaim-probe")]
#[test]
/// 已提交组合的旧文件最终回收失败由原Commit保留；不能取消已发布候选或把它说成未修改。
fn frozen_candidate_retirement_failure_is_reported_by_the_actual_committed_input_receipt() {
    let (_file, document) = fixture("首尾", 1024 * 1024);
    document
        .borrow_mut()
        .set_selection(MarkdownTextSelection::caret(1))
        .unwrap();
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let length = 64 * 1024 + 23;
    let permit = owner.capture(1, &|_| true).unwrap();
    owner
        .set_marked_text(
            &permit,
            &"a".repeat(length),
            NSRange::new(length, 0),
            unspecified(),
            &|_| true,
        )
        .unwrap();
    let observer = document.borrow().backing_observers().last().unwrap();
    let before = owner.state().unwrap();
    let attempts = observer.reclaim_attempts();
    let injected = Cell::new(false);
    let current = |_| {
        // 只在真实唯一候选已经准备后武装旧backing；标记本身不制造错误、不改变队列或正文。
        if !injected.get() && document.borrow().has_prepared_text_input() {
            observer.fail_next_reclaim_for_test();
            injected.set(true);
        }
        true
    };
    let permit = owner.capture(1, &current).unwrap();
    let commit = owner
        .insert_text(&permit, "确认🙂", unspecified(), &current)
        .unwrap();
    assert!(injected.get());
    assert!(observer.reclaim_attempts() > attempts);
    assert_eq!(observer.first_errno(), Some(libc::EIO));
    assert_eq!(observer.mapped_bytes(), 0);
    assert!(observer.unmap_succeeded());
    assert_eq!(commit.state.generation, before.generation + 1);
    assert_eq!(commit.state.selection, MarkdownTextSelection::caret(5));
    assert_eq!(commit.state.marked, None);
    assert_eq!(owner.state().unwrap(), commit.state);
    assert!(!document.borrow().has_prepared_text_input());
    assert!(matches!(
        commit.check_current(&current),
        Err(NativeMarkdownPagedInputError::Document(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    )); // View原finish_commit_ref会把此真实后果包成既有CommittedEffect，禁止重放。
    assert!(matches!(
        commit.check_current(&|_| false),
        Err(NativeMarkdownPagedInputError::Document(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    )); // 同一已提交票的Source首因仍优先于前台撤销，不为断言补领新票。
    let mut units = [0_u16; 4];
    document
        .borrow()
        .text_snapshot()
        .copy_utf16_code_units(1, &mut units)
        .unwrap();
    assert_eq!(String::from_utf16(&units).unwrap(), "确认🙂");
    let backings: Vec<_> = document.borrow().backing_observers().collect();
    owner.close().unwrap(); // close不新增可失败Doc健康门；失败源也必须能够拆图和释放。
    drop((commit, permit, owner, document));
    assert!(
        backings
            .iter()
            .all(|backing| backing.unmap_succeeded() && backing.mapped_bytes() == 0)
    );
    assert_eq!(backings[1].first_errno(), Some(libc::EIO));
}

#[cfg(feature = "native-paged-frontier-probe")]
#[test]
/// 先完成真实Core词组查询和提交，再验证Word与中文组合各有自己的一笔Undo；不是异步IME用例。
fn completed_real_word_delete_precedes_the_single_composition_undo_group() {
    use omni_markdown_core::{
        MarkdownLineComposeBudget, MarkdownLineComposeCancellation, MarkdownLineComposer,
        MarkdownLineFontCatalog, MarkdownWordDeleteAnswer, MarkdownWordDeleteDirection,
    };
    let (_file, document) = fixture("one two", 4096);
    document
        .borrow_mut()
        .set_selection(MarkdownTextSelection::caret(7))
        .unwrap();
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let permit = owner.capture(1, &|_| true).unwrap();
    let source = owner.capture_layout_source(&permit, &|_| true).unwrap();
    let mut composer = MarkdownLineComposer::new(
        source.clone(),
        &source.authority(),
        MarkdownLineFontCatalog::new(),
        640 * 64,
        MarkdownLineComposeCancellation::new(),
    )
    .unwrap();
    let resources = composer.worker_resource_observer();
    composer
        .begin_word_delete(MarkdownWordDeleteDirection::Backward, 7)
        .unwrap();
    let mut ready = None;
    for _ in 0..128 {
        if let MarkdownWordDeleteAnswer::Ready(range) = composer
            .step_word_delete(MarkdownLineComposeBudget::default())
            .unwrap()
            .answer
        {
            ready = Some(range);
            break;
        }
    }
    assert_eq!(ready, Some(4..7));
    drop((composer, source));
    assert!(resources.is_released()); // 真正归还原Word读取后，才允许Input提交原exact范围。
    let range = ready.unwrap();
    owner
        .insert_text(&permit, "", NSRange::new(range.start, range.len()), &|_| {
            true
        })
        .unwrap();
    assert_eq!(text(&document), "one ");
    for text in ["中🙂", "汉😀"] {
        let permit = owner.capture(1, &|_| true).unwrap();
        owner
            .set_marked_text(&permit, text, NSRange::new(1, 2), unspecified(), &|_| true)
            .unwrap();
    }
    let permit = owner.capture(1, &|_| true).unwrap();
    owner
        .insert_text(&permit, "中文", unspecified(), &|_| true)
        .unwrap();
    assert_eq!(text(&document), "one 中文");
    history(&owner, NativeMarkdownPagedInputHistory::Undo);
    assert_eq!(text(&document), "one ");
    history(&owner, NativeMarkdownPagedInputHistory::Undo);
    assert_eq!(text(&document), "one two");
    history(&owner, NativeMarkdownPagedInputHistory::Redo);
    assert_eq!(text(&document), "one ");
    history(&owner, NativeMarkdownPagedInputHistory::Redo);
    assert_eq!(text(&document), "one 中文");
}
