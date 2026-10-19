//! 真实Core写作事务覆盖续写、缩进、普通字素回落、原票取消和共享mmap。

use super::*;
use omni_markdown_core::MarkdownEditingCommand;

#[test]
fn source_return_continues_markdown_and_restores_original_selection_in_one_history() {
    for (input, expected) in [
        ("- 文🙂", "- 文🙂\n- "),
        ("> 文", "> 文\n> "),
        ("1. 文", "1. 文\n2. "),
        ("  文", "  文\n  "),
    ] {
        let (_file, document) = fixture(input, 4096);
        let before = MarkdownTextSelection::caret(input.encode_utf16().count());
        document.borrow_mut().set_selection(before).unwrap();
        let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
        let current = |_| true;
        let original = owner.state().unwrap();
        let permit = owner.capture(1, &current).unwrap();
        let commit = committed(
            owner
                .apply_source_editing_command(
                    &permit,
                    MarkdownEditingCommand::InsertNewline,
                    None,
                    &current,
                )
                .unwrap()
                .expect("Return实际提交完整Core结果"),
        );
        assert_eq!(text(&document), expected);
        assert_eq!(commit.state.generation, original.generation + 1);
        assert_eq!(
            commit.state.selection,
            MarkdownTextSelection::caret(expected.encode_utf16().count())
        );
        assert!(commit.invalidation.characters_changed);
        assert!(matches!(
            owner.apply_source_editing_command(
                &permit,
                MarkdownEditingCommand::InsertNewline,
                None,
                &current
            ),
            Err(NativeMarkdownPagedInputError::Consumed)
        ));
        let permit = owner.capture(1, &current).unwrap();
        let undo = committed(
            owner
                .history(&permit, NativeMarkdownPagedInputHistory::Undo, &current)
                .unwrap(),
        );
        assert_eq!(text(&document), input);
        assert_eq!(undo.state.selection, before);
        assert!(!document.borrow().can_undo());
        let permit = owner.capture(1, &current).unwrap();
        committed(
            owner
                .history(&permit, NativeMarkdownPagedInputHistory::Redo, &current)
                .unwrap(),
        );
        assert_eq!(text(&document), expected);
        assert!(!document.borrow().can_redo());
    }
}

#[test]
fn source_indent_and_backtab_keep_reverse_selection_and_exclude_trailing_line() {
    let (_file, document) = fixture("甲🙂\n乙\n末", 4096);
    let before = MarkdownTextSelection {
        anchor_utf16: 6,
        head_utf16: 0,
    };
    document.borrow_mut().set_selection(before).unwrap();
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let current = |_| true;
    for (command, expected, selection) in [
        (
            MarkdownEditingCommand::IndentMore,
            "  甲🙂\n  乙\n末",
            MarkdownTextSelection {
                anchor_utf16: 10,
                head_utf16: 2,
            },
        ),
        (MarkdownEditingCommand::IndentLess, "甲🙂\n乙\n末", before),
    ] {
        let permit = owner.capture(1, &current).unwrap();
        let commit = committed(
            owner
                .apply_source_editing_command(&permit, command, None, &current)
                .unwrap()
                .expect("实际缩进变化必须交回原事务收据"),
        );
        assert_eq!(text(&document), expected);
        assert_eq!(commit.state.selection, selection);
    }
    let permit = owner.capture(1, &current).unwrap();
    committed(
        owner
            .history(&permit, NativeMarkdownPagedInputHistory::Undo, &current)
            .unwrap(),
    );
    assert_eq!(text(&document), "  甲🙂\n  乙\n末");
    let permit = owner.capture(1, &current).unwrap();
    committed(
        owner
            .history(&permit, NativeMarkdownPagedInputHistory::Undo, &current)
            .unwrap(),
    );
    assert_eq!(text(&document), "甲🙂\n乙\n末");
    assert!(!document.borrow().can_undo()); // 两次真实撤销分别归还取消缩进和缩进，没有额外空事务。
    let stable = owner.state().unwrap();
    let permit = owner.capture(1, &current).unwrap();
    assert!(matches!(
        owner
            .apply_source_editing_command(&permit, MarkdownEditingCommand::IndentLess, None, &current)
            .unwrap(),
        Some(NativeMarkdownPagedInputOutcome::Unchanged(state)) if state == stable
    ));
    assert_eq!(owner.state().unwrap(), stable);
    assert_eq!(text(&document), "甲🙂\n乙\n末");
    assert!(document.borrow().can_redo()); // 已无缩进的BackTab不插假历史，也不破坏既有Redo。
}

#[test]
fn source_markup_backspace_and_real_grapheme_fallback_consume_the_same_permit() {
    for (input, head, ordinary, expected, after) in [
        ("- 文", 2, None, "文", 0),
        ("> 文", 2, None, "文", 0),
        ("甲🙂", 3, Some(NSRange::new(1, 2)), "甲", 1),
    ] {
        let (_file, document) = fixture(input, 4096);
        document
            .borrow_mut()
            .set_selection(MarkdownTextSelection::caret(head))
            .unwrap();
        let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
        let current = |_| true;
        let permit = owner.capture(1, &current).unwrap();
        let commit = committed(
            owner
                .apply_source_editing_command(
                    &permit,
                    MarkdownEditingCommand::DeleteMarkupBackward,
                    ordinary,
                    &current,
                )
                .unwrap()
                .expect("标记或原GCB必须实际完成一次删除"),
        );
        assert_eq!(text(&document), expected);
        assert_eq!(commit.state.selection, MarkdownTextSelection::caret(after));
        assert!(matches!(
            owner.insert_text(&permit, "", NSRange::new(0, 0), &current),
            Err(NativeMarkdownPagedInputError::Consumed)
        ));
        let permit = owner.capture(1, &current).unwrap();
        let undo = committed(
            owner
                .history(&permit, NativeMarkdownPagedInputHistory::Undo, &current)
                .unwrap(),
        );
        assert_eq!(text(&document), input);
        assert_eq!(undo.state.selection, MarkdownTextSelection::caret(head));
        assert!(!document.borrow().can_undo()); // 普通GCB回落不另建一次标记删除历史。
    }

    let (_file, document) = fixture("甲🙂", 4096);
    document
        .borrow_mut()
        .set_selection(MarkdownTextSelection::caret(3))
        .unwrap();
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let before = owner.state().unwrap();
    let permit = owner.capture(1, &|_| true).unwrap();
    assert!(
        owner
            .apply_source_editing_command(
                &permit,
                MarkdownEditingCommand::DeleteMarkupBackward,
                None,
                &|_| true
            )
            .unwrap()
            .is_none()
    );
    assert_eq!(owner.state().unwrap(), before);
    assert_eq!(text(&document), "甲🙂");
    assert!(!document.borrow().can_undo()); // None是尚缺真实GCB，绝不是已成功删除或空历史。
}

#[test]
fn source_commands_preserve_marked_text_and_all_multiple_ranges_on_rejection() {
    let (_file, document) = fixture("甲🙂\n乙", 4096);
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let current = |_| true;
    let permit = owner.capture(1, &current).unwrap();
    committed(
        owner
            .set_selected_ranges(&permit, &[0..1, 4..5], &current)
            .unwrap(),
    );
    let multiple = owner.state().unwrap();
    for command in [
        MarkdownEditingCommand::InsertNewline,
        MarkdownEditingCommand::IndentMore,
        MarkdownEditingCommand::IndentLess,
        MarkdownEditingCommand::DeleteMarkupBackward,
    ] {
        let permit = owner.capture(1, &current).unwrap();
        assert!(matches!(
            owner.apply_source_editing_command(&permit, command, None, &current),
            Err(NativeMarkdownPagedInputError::MultipleSelectionInput)
        ));
        assert_eq!(owner.state().unwrap(), multiple);
        let permit = owner.capture(1, &current).unwrap();
        assert_eq!(
            owner.selected_ranges(&permit, &current).unwrap(),
            vec![0..1, 4..5]
        );
        assert_eq!(text(&document), "甲🙂\n乙");
        assert!(!document.borrow().can_undo());
    }
    let permit = owner.capture(1, &current).unwrap();
    committed(
        owner
            .set_selection(&permit, MarkdownTextSelection::caret(0), &current)
            .unwrap(),
    );
    let permit = owner.capture(1, &current).unwrap();
    owner
        .set_marked_text(&permit, "中", NSRange::new(1, 0), unspecified(), &current)
        .unwrap();
    let marked = owner.state().unwrap();
    for command in [
        MarkdownEditingCommand::InsertNewline,
        MarkdownEditingCommand::IndentMore,
    ] {
        let permit = owner.capture(1, &current).unwrap();
        assert!(matches!(
            owner.apply_source_editing_command(&permit, command, None, &current),
            Err(NativeMarkdownPagedInputError::CompositionActive)
        ));
        assert_eq!(owner.state().unwrap(), marked);
        assert_eq!(text(&document), "中甲🙂\n乙");
    }
    let permit = owner.capture(1, &current).unwrap();
    committed(owner.unmark(&permit, &current).unwrap());
    let permit = owner.capture(1, &current).unwrap();
    committed(
        owner
            .history(&permit, NativeMarkdownPagedInputHistory::Undo, &current)
            .unwrap(),
    );
    assert_eq!(text(&document), "甲🙂\n乙");
    assert!(!document.borrow().can_undo()); // 拒绝的源码命令未插在原marked输入之前或之后。
}

#[test]
fn source_ordinary_backspace_cancels_prepared_bytes_when_the_original_scene_loses_permission() {
    let (_file, document) = fixture("甲🙂", 4096);
    document
        .borrow_mut()
        .set_selection(MarkdownTextSelection::caret(3))
        .unwrap();
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let before = owner.state().unwrap();
    let prepared_seen = Cell::new(false);
    let current = |_| {
        let document = document.try_borrow_mut().expect("current跨了原Doc借用");
        assert!(owner.folds.try_borrow_mut().is_ok());
        if document.has_prepared_text_input() {
            prepared_seen.set(true);
        } // 真正的普通GCB替换候选已建立后撤票；不能只测尚未开始的false。
        !prepared_seen.get()
    };
    let permit = owner.capture(1, &current).unwrap();
    assert!(matches!(
        owner.apply_source_editing_command(
            &permit,
            MarkdownEditingCommand::DeleteMarkupBackward,
            Some(NSRange::new(1, 2)),
            &current
        ),
        Err(NativeMarkdownPagedInputError::Revoked)
    ));
    assert!(prepared_seen.get());
    assert_eq!(owner.state().unwrap(), before);
    assert_eq!(text(&document), "甲🙂");
    assert!(!document.borrow().has_prepared_text_input());
    assert!(!document.borrow().can_undo());
    assert!(!owner.is_busy());
}

#[test]
fn source_command_capacity_and_current_rejections_keep_document_history_and_folds() {
    let (_file, document) = fixture("- 文", 6);
    document
        .borrow_mut()
        .set_selection(MarkdownTextSelection::caret(3))
        .unwrap();
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let before = owner.state().unwrap();
    let permit = owner.capture(1, &|_| true).unwrap();
    assert!(
        owner
            .apply_source_editing_command(
                &permit,
                MarkdownEditingCommand::InsertNewline,
                None,
                &|_| true
            )
            .is_err()
    );
    assert_eq!(owner.state().unwrap(), before);
    assert_eq!(text(&document), "- 文");
    assert!(!document.borrow().can_undo() && !document.borrow().can_redo());
    assert!(!document.borrow().has_prepared_text_input());

    let (_file, document) = fixture("# H\nbody\nend", 4096);
    let owner = NativeMarkdownPagedInput::with_folds(
        Rc::downgrade(&document),
        vec![MarkdownFoldRegion {
            header_line: 0,
            hidden_utf16_range: 3..8,
        }],
    )
    .unwrap();
    let current = |_| {
        assert!(document.try_borrow_mut().is_ok());
        assert!(owner.folds.try_borrow_mut().is_ok());
        true
    };
    let permit = owner.capture(1, &current).unwrap();
    let commit = committed(
        owner
            .apply_source_editing_command(
                &permit,
                MarkdownEditingCommand::IndentMore,
                None,
                &current,
            )
            .unwrap()
            .unwrap(),
    );
    assert!(commit.invalidation.folds_changed);
    assert_eq!(
        &*owner.folds.borrow(),
        &[MarkdownFoldRegion {
            header_line: 0,
            hidden_utf16_range: 5..10
        }]
    );
    let stable = owner.state().unwrap();
    let current = |_| {
        assert!(document.try_borrow_mut().is_ok());
        assert!(owner.folds.try_borrow_mut().is_ok());
        !owner.is_busy()
    };
    let permit = owner.capture(1, &current).unwrap();
    assert!(matches!(
        owner.apply_source_editing_command(
            &permit,
            MarkdownEditingCommand::IndentLess,
            None,
            &current
        ),
        Err(NativeMarkdownPagedInputError::Revoked)
    ));
    assert_eq!(owner.state().unwrap(), stable);
    assert_eq!(text(&document), "  # H\nbody\nend");
    assert!(!owner.is_busy());
}

#[test]
fn source_indent_of_a_giant_single_paragraph_preserves_saved_bytes_and_releases_mapping() {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = tempfile::NamedTempFile::new().unwrap();
    let block = [b'a'; 4096];
    for _ in 0..256 {
        file.write_all(&block).unwrap(); // 真实1MiB单段落按固定块生成，不创建全文String。
    }
    let mut document = MarkdownDocumentBuffer::open(file.path(), 2 * 1024 * 1024).unwrap();
    document
        .set_selection(MarkdownTextSelection::caret(1024 * 1024))
        .unwrap();
    let mapping = document.original_backing_observer().unwrap();
    let document = Rc::new(RefCell::new(document));
    let weak = Rc::downgrade(&document);
    let owner = NativeMarkdownPagedInput::new(weak.clone()).unwrap();
    let permit = owner.capture(1, &|_| true).unwrap();
    let commit = committed(
        owner
            .apply_source_editing_command(
                &permit,
                MarkdownEditingCommand::IndentMore,
                None,
                &|_| true,
            )
            .unwrap()
            .unwrap(),
    );
    let mut prefix = [0_u16; 3];
    document
        .borrow()
        .copy_utf16_code_units(0, &mut prefix)
        .unwrap();
    assert_eq!(prefix, [0x20, 0x20, 0x61]);
    assert_eq!(commit.state.utf16_len, 1024 * 1024 + 2);
    assert_eq!(
        commit.state.selection,
        MarkdownTextSelection::caret(1024 * 1024 + 2)
    );
    assert_eq!(document.borrow().byte_len(), 1024 * 1024 + 2);
    assert_eq!(mapping.mapped_bytes(), 1024 * 1024);
    // 直接使用生产保存入口核完整字节：两个空格后仍是全部原文，每次只保留一个4KiB块。
    let mut saved = tempfile::tempfile().unwrap();
    document.borrow().write_to(&mut saved).unwrap();
    saved.seek(SeekFrom::Start(0)).unwrap();
    let mut prefix = [0_u8; 2];
    saved.read_exact(&mut prefix).unwrap();
    assert_eq!(prefix, *b"  ");
    let mut scratch = [0_u8; 4096];
    for _ in 0..256 {
        saved.read_exact(&mut scratch).unwrap();
        assert_eq!(scratch, block);
    }
    assert_eq!(saved.read(&mut scratch).unwrap(), 0);
    drop(saved);
    let permit = owner.capture(1, &|_| true).unwrap();
    let undo = committed(
        owner
            .history(&permit, NativeMarkdownPagedInputHistory::Undo, &|_| true)
            .unwrap(),
    );
    assert_eq!(document.borrow().byte_len(), 1024 * 1024);
    assert_eq!(
        undo.state.selection,
        MarkdownTextSelection::caret(1024 * 1024)
    );
    assert!(!document.borrow().can_undo());
    assert!(mapping.peak_managed_resident_bytes() <= 64 * 1024);
    drop(document);
    assert!(weak.upgrade().is_none());
    assert!(mapping.unmap_succeeded() && !mapping.failed());
    assert_eq!(mapping.mapped_bytes(), 0);
    assert!(commit.check_current(&|_| true).is_err()); // 收据和已消费permit均不保活Doc或原映射。
}
