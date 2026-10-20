//! 原Input格式用例直接运行Core事务，核真实Unicode选区、历史、失败回退与共享映射。

use super::*;
use omni_markdown_core::MarkdownFormatAction as Format;

/// 行内inner与块格式分别沿Core选择规则，原反向before只由一次真实Undo恢复。
#[test]
fn source_format_body_preserves_core_selection_and_exact_history() {
    for (action, expected, after) in [
        (
            Format::Bold,
            "前**中😀**后",
            MarkdownTextSelection {
                anchor_utf16: 3,
                head_utf16: 6,
            },
        ),
        (
            Format::Link,
            "前[中😀](https://)后",
            MarkdownTextSelection {
                anchor_utf16: 2,
                head_utf16: 5,
            },
        ),
        (
            Format::Underline,
            "前<u>中😀</u>后",
            MarkdownTextSelection {
                anchor_utf16: 4,
                head_utf16: 7,
            },
        ),
    ] {
        let (_file, document) = fixture("前中😀后", 4096);
        let before = MarkdownTextSelection {
            anchor_utf16: 4,
            head_utf16: 1,
        };
        document.borrow_mut().set_selection(before).unwrap();
        let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
        let current = |_| {
            assert!(document.try_borrow_mut().is_ok());
            assert!(owner.folds.try_borrow_mut().is_ok());
            true
        }; // 任何可重入的权限闭包都必须在Doc/fold借用外执行。
        let original = owner.state().unwrap();
        let permit = owner.capture(1, &current).unwrap();
        let commit = owner.apply_format(&permit, action, &current).unwrap();
        assert_eq!(text(&document), expected);
        assert_eq!(commit.state.generation, original.generation + 1);
        // 行内包裹沿原Core约定正向选择inner，不能在Native事后改回方向而破坏历史after。
        assert_eq!(commit.state.selection, after);
        assert_eq!(document.borrow().selection(), after);
        assert!(commit.invalidation.characters_changed);
        commit.check_current(&current).unwrap();
        assert!(matches!(
            owner.apply_format(&permit, action, &current),
            Err(NativeMarkdownPagedInputError::Consumed)
        ));
        let undo = owner.capture(1, &current).unwrap();
        let undo = committed(
            owner
                .history(&undo, NativeMarkdownPagedInputHistory::Undo, &current)
                .unwrap(),
        );
        assert_eq!(text(&document), "前中😀后");
        assert_eq!(undo.state.selection, before); // 唯一Undo保留菜单点击前的真实反向选择。
        assert!(!document.borrow().can_undo());
        let redo = owner.capture(1, &current).unwrap();
        let redo = committed(
            owner
                .history(&redo, NativeMarkdownPagedInputHistory::Redo, &current)
                .unwrap(),
        );
        assert_eq!(text(&document), expected);
        assert_eq!(redo.state.selection, after);
        assert!(!document.borrow().can_redo());
    }
    for (action, expected, after) in [
        (
            Format::Heading2,
            "## 甲\n乙",
            MarkdownTextSelection {
                anchor_utf16: 6,
                head_utf16: 3,
            },
        ),
        (
            Format::BulletList,
            "- 甲\n- 乙",
            MarkdownTextSelection {
                anchor_utf16: 7,
                head_utf16: 2,
            },
        ),
    ] {
        let (_file, document) = fixture("甲\n乙", 4096);
        let before = MarkdownTextSelection {
            anchor_utf16: 3,
            head_utf16: 0,
        };
        document.borrow_mut().set_selection(before).unwrap();
        let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
        let permit = owner.capture(1, &|_| true).unwrap();
        let commit = owner.apply_format(&permit, action, &|_| true).unwrap();
        assert_eq!(text(&document), expected);
        assert_eq!(commit.state.selection, after); // 块格式原本保方向，适配层也必须保原反向after。
        let undo = owner.capture(1, &|_| true).unwrap();
        let undo = committed(
            owner
                .history(&undo, NativeMarkdownPagedInputHistory::Undo, &|_| true)
                .unwrap(),
        );
        assert_eq!(text(&document), "甲\n乙");
        assert_eq!(undo.state.selection, before);
        assert!(!document.borrow().can_undo());
    }
}

/// 空caret只使用原占位与模板选区，不把格式当作另一次输入后再修正选择。
#[test]
fn source_format_caret_uses_original_placeholder_and_template_selection() {
    for (action, expected, selection) in [
        (
            Format::Bold,
            "前**加粗文字**尾",
            MarkdownTextSelection {
                anchor_utf16: 3,
                head_utf16: 7,
            },
        ),
        (
            Format::Link,
            "前[链接文字](https://)尾",
            MarkdownTextSelection {
                anchor_utf16: 2,
                head_utf16: 6,
            },
        ),
        (
            Format::CodeBlock,
            "前\n```\ncode\n```\n尾",
            MarkdownTextSelection {
                anchor_utf16: 6,
                head_utf16: 10,
            },
        ),
        (
            Format::Table,
            "前\n| 列 1 | 列 2 |\n| --- | --- |\n| 内容 | 内容 |\n尾",
            MarkdownTextSelection {
                anchor_utf16: 4,
                head_utf16: 7,
            },
        ),
    ] {
        let (_file, document) = fixture("前尾", 4096);
        document
            .borrow_mut()
            .set_selection(MarkdownTextSelection::caret(1))
            .unwrap();
        let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
        let permit = owner.capture(1, &|_| true).unwrap();
        let commit = owner.apply_format(&permit, action, &|_| true).unwrap();
        assert_eq!(text(&document), expected);
        assert_eq!(commit.state.selection, selection);
        let undo = owner.capture(1, &|_| true).unwrap();
        let undo = committed(
            owner
                .history(&undo, NativeMarkdownPagedInputHistory::Undo, &|_| true)
                .unwrap(),
        );
        assert_eq!(text(&document), "前尾");
        assert_eq!(undo.state.selection, MarkdownTextSelection::caret(1));
        assert!(!document.borrow().can_undo()); // 空caret模板也只有一笔canonical格式历史。
    }
}

/// 真实两端插入保留内部fold；候选observer拒绝标量错误时正文与历史一起回退。
#[test]
fn source_format_maps_actual_surround_changes_and_rolls_back_observer_failure() {
    let (_file, document) = fixture("# H\nbody\nend", 4096);
    document
        .borrow_mut()
        .set_selection(MarkdownTextSelection {
            anchor_utf16: 12,
            head_utf16: 0,
        })
        .unwrap();
    let owner = NativeMarkdownPagedInput::with_folds(
        Rc::downgrade(&document),
        vec![MarkdownFoldRegion {
            header_line: 0,
            hidden_utf16_range: 3..8,
        }],
    )
    .unwrap();
    let permit = owner.capture(1, &|_| true).unwrap();
    let commit = owner
        .apply_format(&permit, Format::Bold, &|_| true)
        .unwrap();
    assert_eq!(text(&document), "**# H\nbody\nend**");
    assert!(commit.invalidation.folds_changed);
    assert_eq!(
        &*owner.folds.borrow(),
        &[MarkdownFoldRegion {
            header_line: 0,
            hidden_utf16_range: 5..10
        }]
    );
    // 真实变化只有两端插入；若错误使用整段envelope，内部fold会被删掉而非平移。
    let undo = owner.capture(1, &|_| true).unwrap();
    committed(
        owner
            .history(&undo, NativeMarkdownPagedInputHistory::Undo, &|_| true)
            .unwrap(),
    );
    assert_eq!(text(&document), "# H\nbody\nend");
    assert_eq!(
        &*owner.folds.borrow(),
        &[MarkdownFoldRegion {
            header_line: 0,
            hidden_utf16_range: 3..8
        }]
    );

    let (_file, document) = fixture("😀", 4096);
    document
        .borrow_mut()
        .set_selection(MarkdownTextSelection {
            anchor_utf16: 0,
            head_utf16: 2,
        })
        .unwrap();
    // 映射器只验整数范围；真实candidate observer还必须拒绝落在代理项内部的旧折叠端点。
    let owner = NativeMarkdownPagedInput::with_folds(
        Rc::downgrade(&document),
        vec![MarkdownFoldRegion {
            header_line: 0,
            hidden_utf16_range: 1..2,
        }],
    )
    .unwrap();
    let before = owner.state().unwrap();
    let folds = owner.folds.borrow().clone();
    let permit = owner.capture(1, &|_| true).unwrap();
    assert!(matches!(
        owner.apply_format(&permit, Format::Bold, &|_| true),
        Err(NativeMarkdownPagedInputError::Edit(
            MarkdownObservedEditError::Observer(NativeMarkdownPagedInputObserverError::Document(_))
        ))
    ));
    assert_eq!(owner.state().unwrap(), before);
    assert_eq!(text(&document), "😀");
    assert_eq!(*owner.folds.borrow(), folds);
    assert!(!document.borrow().can_undo() && !document.borrow().can_redo());
    assert!(!document.borrow().has_prepared_text_input());
    assert!(!owner.is_busy()); // observer拒绝后，原新增marker候选和Mutation占用均已真实归还。
}

/// 格式前置拒绝保原IME、多选及预算，不从主memento或较新权限补一次提交。
#[test]
fn source_format_rejects_marked_multiple_ranges_stale_permit_and_budget_without_edit() {
    let (_file, document) = fixture("甲🙂乙", 4096);
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let permit = owner.capture(1, &|_| true).unwrap();
    committed(
        owner
            .set_selected_ranges(&permit, &[0..1, 3..4], &|_| true)
            .unwrap(),
    );
    let before = owner.state().unwrap();
    let permit = owner.capture(1, &|_| true).unwrap();
    assert!(matches!(
        owner.apply_format(&permit, Format::Bold, &|_| true),
        Err(NativeMarkdownPagedInputError::MultipleSelectionInput)
    ));
    assert_eq!(owner.state().unwrap(), before);
    let read = owner.capture(1, &|_| true).unwrap();
    assert_eq!(
        &*owner.selected_ranges(&read, &|_| true).unwrap(),
        &[0..1, 3..4]
    );
    assert_eq!(text(&document), "甲🙂乙");
    assert!(!document.borrow().can_undo());

    let (_file, document) = fixture("甲", 4096);
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let permit = owner.capture(1, &|_| true).unwrap();
    owner
        .set_marked_text(&permit, "中", NSRange::new(1, 0), unspecified(), &|_| true)
        .unwrap();
    let before = owner.state().unwrap();
    let history = (document.borrow().can_undo(), document.borrow().can_redo());
    let permit = owner.capture(1, &|_| true).unwrap();
    assert!(matches!(
        owner.apply_format(&permit, Format::Bold, &|_| true),
        Err(NativeMarkdownPagedInputError::CompositionActive)
    ));
    assert_eq!(owner.state().unwrap(), before);
    assert_eq!(text(&document), "中甲");
    assert_eq!(
        (document.borrow().can_undo(), document.borrow().can_redo()),
        history
    );

    let (_file, document) = fixture("甲🙂", 7);
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let before = owner.state().unwrap();
    let permit = owner.capture(1, &|_| true).unwrap();
    assert!(
        owner
            .apply_format(&permit, Format::Bold, &|_| true)
            .is_err()
    ); // 原live-byte上限不因格式marker很短而放宽。
    assert_eq!(owner.state().unwrap(), before);
    assert_eq!(text(&document), "甲🙂");
    assert!(!document.borrow().can_undo() && !document.borrow().can_redo());
    let stale = owner.capture(1, &|_| true).unwrap();
    assert!(matches!(
        owner.apply_format(&stale, Format::InlineCode, &|_| false),
        Err(NativeMarkdownPagedInputError::Revoked)
    ));
    assert_eq!(owner.state().unwrap(), before);
}

/// 已提交收据的显示权限撤销不改变canonical事实，旧permit也不能重放格式。
#[test]
fn source_format_committed_receipt_revocation_does_not_replay_or_rollback() {
    let (_file, document) = fixture("甲", 4096);
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let epoch = Cell::new(1);
    let current = |expected| expected == epoch.get();
    let permit = owner.capture(1, &current).unwrap();
    let commit = owner.apply_format(&permit, Format::Bold, &current).unwrap();
    epoch.set(2); // 模拟Core已提交后原窗口权限结束；收据只停止后果，不撤销已发布正文。
    assert!(matches!(
        commit.check_current(&current),
        Err(NativeMarkdownPagedInputError::Revoked)
    ));
    assert_eq!(text(&document), "**加粗文字**甲");
    assert_eq!(document.borrow().generation(), 1);
    assert!(matches!(
        owner.apply_format(&permit, Format::Bold, &current),
        Err(NativeMarkdownPagedInputError::Consumed)
    ));
    assert_eq!(document.borrow().generation(), 1);
    let undo = owner.capture(2, &current).unwrap();
    let undo = committed(
        owner
            .history(&undo, NativeMarkdownPagedInputHistory::Undo, &current)
            .unwrap(),
    );
    assert_eq!(text(&document), "甲");
    assert_eq!(undo.state.selection, MarkdownTextSelection::caret(0));
    assert!(!document.borrow().can_undo());
}

/// 真实Cell格式只发布原局部root/历史；marked、块位置、旧票和Source失败不改canonical。
#[test]
fn source_format_cell_inline_history_and_rejections_preserve_local_domain_and_canonical() {
    use omni_markdown_core::MarkdownCellDraftError;
    use omni_markdown_core::markdown_document_buffer::{
        MarkdownTableWindowStep, MarkdownTableWindowTarget,
    };
    let input = "| H | C |\n| --- | --- |\n| base | other |\n";
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(input.as_bytes()).unwrap();
    let tail = [b'x'; 4096];
    for _ in 0..32 {
        file.write_all(&tail).unwrap();
    } // 原映射超过64KiB驻留槽，后续有限读才能触发真实驱逐，而非直接伪置Source错误。
    let document = Rc::new(RefCell::new(
        MarkdownDocumentBuffer::open(file.path(), 256 * 1024).unwrap(),
    ));
    let assert_canonical_bytes = || {
        let original = document.borrow();
        assert_eq!(original.byte_len(), input.len() + 32 * tail.len());
        let mut prefix = [0; 64];
        original
            .copy_utf16_code_units(0, &mut prefix[..input.len()])
            .unwrap();
        assert!(
            prefix[..input.len()]
                .iter()
                .copied()
                .eq(input.bytes().map(u16::from))
        ); // 固定短表保留全部原字节；测试不建立大canonical镜像。
        let mut offset = input.len();
        let mut actual = [0; 4096];
        while offset < original.utf16_len() {
            let count = actual.len().min(original.utf16_len() - offset);
            original
                .copy_utf16_code_units(offset, &mut actual[..count])
                .unwrap();
            assert!(actual[..count].iter().all(|unit| *unit == u16::from(b'x')));
            offset += count;
        } // 每次仅借用固定窗口，完整核尾部未被局部格式或本地历史改写。
    };
    let verified = {
        let document = document.borrow();
        let mut request = document
            .table_window_request(
                MarkdownTableWindowTarget::AtUtf16(0),
                0..input.encode_utf16().count(), // 只请求原短表；尾部仅供真实页驱逐，不属于表格视口。
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
                other => panic!("格式控制的真实表格窗口未就绪：{other:?}"),
            }
        }
    };
    let source = document
        .borrow()
        .verified_table_cell_source(&verified, 1, 0)
        .unwrap();
    let session = NativeMarkdownPagedCellSession::begin(Rc::downgrade(&document), source).unwrap();
    let resources = session.resource_observer().unwrap();
    let owner = NativeMarkdownPagedInput::new_cell(Rc::downgrade(&session)).unwrap();
    let permit = owner.capture(1, &|_| true).unwrap();
    owner
        .set_marked_text(&permit, "中", NSRange::new(1, 0), unspecified(), &|_| true)
        .unwrap();
    let before = owner.state().unwrap();
    let permit = owner.capture(1, &|_| true).unwrap();
    assert!(matches!(
        owner.apply_format(&permit, Format::Bold, &|_| true),
        Err(NativeMarkdownPagedInputError::CompositionActive)
    ));
    assert_eq!(owner.state().unwrap(), before);
    let read = owner.capture(1, &|_| true).unwrap();
    let mut units = [0; 5];
    owner
        .copy_utf16_code_units(&read, 0, &mut units, &|_| true)
        .unwrap();
    assert_eq!(units, [0x4e2d, 0x62, 0x61, 0x73, 0x65]);
    assert_canonical_bytes();
    assert_eq!(document.borrow().generation(), 0);
    assert!(!document.borrow().can_undo());
    let permit = owner.capture(1, &|_| true).unwrap();
    owner.unmark(&permit, &|_| true).unwrap();
    let undo = owner.capture(1, &|_| true).unwrap();
    committed(
        owner
            .history(&undo, NativeMarkdownPagedInputHistory::Undo, &|_| true)
            .unwrap(),
    );
    let read = owner.capture(1, &|_| true).unwrap();
    let mut units = [0; 4];
    owner
        .copy_utf16_code_units(&read, 0, &mut units, &|_| true)
        .unwrap();
    assert_eq!(units, [0x62, 0x61, 0x73, 0x65]); // 拒绝格式没有插入额外本地Undo，也没有提交canonical。
    let permit = owner.capture(1, &|_| true).unwrap();
    owner
        .insert_text(&permit, "前中😀后", NSRange::new(0, 4), &|_| true)
        .unwrap();
    let before_selection = MarkdownTextSelection {
        anchor_utf16: 4,
        head_utf16: 1,
    };
    let permit = owner.capture(1, &|_| true).unwrap();
    owner
        .set_selection(&permit, before_selection, &|_| true)
        .unwrap();
    let canonical_state = || {
        let original = document.borrow();
        (
            original.generation(),
            original.byte_len(),
            original.selection(),
            original.can_undo(),
            original.can_redo(),
        )
    }; // 只核当前正式Core公开的canonical事实，不为适配器测试增设存储统计入口。
    let baseline = canonical_state();
    let current = |_| {
        assert!(document.try_borrow_mut().is_ok());
        assert!(owner.folds.try_borrow_mut().is_ok());
        true
    }; // 外部权限回调始终在Core借用外执行；Cell成功发布也不能事后回调并伪装成拒绝。
    let original_source = session.capture_source().unwrap();
    let permit = owner.capture(1, &current).unwrap();
    let commit = owner.apply_format(&permit, Format::Bold, &current).unwrap();
    assert_eq!(commit.state.utf16_len, 9);
    assert_eq!(
        commit.state.selection,
        MarkdownTextSelection {
            anchor_utf16: 3,
            head_utf16: 6
        }
    );
    assert_eq!(commit.invalidation.replaced_utf16_range, Some(1..4));
    assert_eq!(commit.invalidation.change_in_utf16_length, 4);
    assert!(commit.invalidation.characters_changed && !commit.invalidation.folds_changed);
    commit.check_current(&current).unwrap();
    assert!(owner.document_identity().upgrade().is_none());
    assert_eq!(
        original_source.byte_offset_for_utf16(0),
        Err(MarkdownDocumentBufferError::PreparedTextInputStale)
    ); // 原Cell Source按真实content_serial撤权，不能借空DocWeak假装改了正文。
    drop(original_source);
    let read = owner.capture(1, &current).unwrap();
    let mut formatted = [0; 9];
    owner
        .copy_utf16_code_units(&read, 0, &mut formatted, &current)
        .unwrap();
    assert_eq!(
        formatted,
        [
            0x524d, 0x2a, 0x2a, 0x4e2d, 0xd83d, 0xde00, 0x2a, 0x2a, 0x540e
        ]
    );
    assert!(matches!(
        owner.apply_format(&permit, Format::Bold, &current),
        Err(NativeMarkdownPagedInputError::Consumed)
    ));
    let undo = owner.capture(1, &current).unwrap();
    let undo = committed(
        owner
            .history(&undo, NativeMarkdownPagedInputHistory::Undo, &current)
            .unwrap(),
    );
    assert_eq!(undo.state.utf16_len, 5);
    assert_eq!(undo.state.selection, before_selection);
    let read = owner.capture(1, &current).unwrap();
    let mut raw = [0; 5];
    owner
        .copy_utf16_code_units(&read, 0, &mut raw, &current)
        .unwrap();
    assert_eq!(raw, [0x524d, 0x4e2d, 0xd83d, 0xde00, 0x540e]);
    let selection = owner.capture(1, &current).unwrap();
    owner
        .set_selection(&selection, MarkdownTextSelection::caret(0), &current)
        .unwrap();
    let redo = owner.capture(1, &current).unwrap();
    let redo = committed(
        owner
            .history(&redo, NativeMarkdownPagedInputHistory::Redo, &current)
            .unwrap(),
    );
    assert_eq!(redo.state.selection, commit.state.selection);
    assert_eq!(redo.state.utf16_len, 9); // caret移动不能改写原Isolated格式组的真实after。
    assert_eq!(canonical_state(), baseline);
    assert_canonical_bytes();
    for action in [
        Format::Heading1,
        Format::Heading2,
        Format::Heading3,
        Format::BulletList,
        Format::OrderedList,
        Format::TaskList,
        Format::Blockquote,
        Format::Divider,
        Format::CodeBlock,
        Format::FormulaBlock,
        Format::Table,
    ] {
        let before = owner.state().unwrap();
        let permit = owner.capture(1, &current).unwrap();
        assert!(matches!(
            owner.apply_format(&permit, action, &current),
            Err(NativeMarkdownPagedInputError::Cell(
                MarkdownCellDraftError::FormatRequiresDocument
            ))
        ));
        assert_eq!(owner.state().unwrap(), before);
    } // 这里只回报位置错误；不能为支持正文命令而暗中提交Cell或丢掉这些原菜单动作。
    let before = owner.state().unwrap();
    let revoked = owner.capture(1, &current).unwrap();
    assert!(matches!(
        owner.apply_format(&revoked, Format::Heading1, &|_| false),
        Err(NativeMarkdownPagedInputError::Revoked)
    ));
    assert_eq!(owner.state().unwrap(), before);
    let stale = owner.capture(1, &current).unwrap();
    let selection = owner.capture(1, &current).unwrap();
    owner
        .set_selection(&selection, MarkdownTextSelection::caret(0), &current)
        .unwrap();
    let before = owner.state().unwrap();
    assert!(matches!(
        owner.apply_format(&stale, Format::Heading1, &current),
        Err(NativeMarkdownPagedInputError::Cell(
            MarkdownCellDraftError::Stale
        ))
    )); // 原edit serial失效先于新的格式位置判断，不能重捕当前caret把旧命令续上。
    assert_eq!(owner.state().unwrap(), before);
    assert_eq!(canonical_state(), baseline);
    assert_canonical_bytes();
    #[cfg(feature = "native-source-reclaim-probe")]
    {
        let original = owner.capture(1, &current).unwrap();
        let backing = document.borrow().original_backing_observer().unwrap();
        let before = (backing.reclaim_attempts(), backing.reclaim_successes());
        backing.fail_next_reclaim_for_test();
        let mut unit = [0];
        assert_eq!(
            document.borrow().copy_utf16_code_units(0, &mut unit),
            Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                libc::EIO
            ))
        );
        assert_eq!(backing.first_errno(), Some(libc::EIO));
        assert!(backing.reclaim_attempts() > before.0 && backing.reclaim_successes() > before.1);
        // 前一轮完整有限oracle已读到尾部；回读首个窗口必须驱逐真实原页。
        // observer只在实际msync成功后注入合成EIO，不把它称作内核故障。
        assert!(matches!(
            owner.apply_format(&original, Format::Heading1, &|_| false),
            Err(NativeMarkdownPagedInputError::Cell(
                MarkdownCellDraftError::Document(
                    MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
                )
            ))
        )); // 真实Source EIO先于撤权/块位置，输入拒绝不能被误报成普通菜单失败。
        assert_eq!(resources.snapshot().first_source_errno, Some(libc::EIO));
        assert_eq!(document.borrow().generation(), 0);
        assert_eq!(canonical_state(), baseline);
    }
    owner.close().unwrap();
    session.close().unwrap();
    assert!(resources.is_released());
    {
        use std::io::Read;
        let mut original_file = std::fs::File::open(file.path()).unwrap();
        let mut prefix = [0; 64];
        original_file
            .read_exact(&mut prefix[..input.len()])
            .unwrap();
        assert_eq!(&prefix[..input.len()], input.as_bytes());
        let mut actual = [0; 4096];
        for _ in 0..32 {
            original_file.read_exact(&mut actual).unwrap();
            assert_eq!(actual, tail);
        }
        assert_eq!(original_file.read(&mut actual).unwrap(), 0);
    } // 原文件逐块完整比较，源失败与局部取消都不得改写用户持久数据。
}

/// 大选区只增加有限marker并共享原映射，完整保存逐块核验后确认原资源真实释放。
#[test]
fn source_format_large_surround_keeps_shared_mapping_and_exact_saved_bytes() {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = tempfile::NamedTempFile::new().unwrap();
    let block = [b'x'; 4096];
    for _ in 0..256 {
        file.write_all(&block).unwrap(); // 样本与完整保存oracle都按固定4KiB处理，不保留整篇String。
    }
    let mut original = MarkdownDocumentBuffer::open(file.path(), 2 * 1024 * 1024).unwrap();
    let before = MarkdownTextSelection {
        anchor_utf16: 1024 * 1024,
        head_utf16: 0,
    };
    original.set_selection(before).unwrap();
    let mapping = original.original_backing_observer().unwrap();
    let document = Rc::new(RefCell::new(original));
    let weak = Rc::downgrade(&document);
    let owner = NativeMarkdownPagedInput::new(weak.clone()).unwrap();
    let permit = owner.capture(1, &|_| true).unwrap();
    let commit = owner
        .apply_format(&permit, Format::Bold, &|_| true)
        .unwrap();
    assert_eq!(commit.state.utf16_len, 1024 * 1024 + 4);
    assert_eq!(
        commit.state.selection,
        MarkdownTextSelection {
            anchor_utf16: 2,
            head_utf16: 1024 * 1024 + 2
        }
    );
    assert_eq!(document.borrow().byte_len(), 1024 * 1024 + 4);
    assert_eq!(mapping.mapped_bytes(), 1024 * 1024);
    let mut saved = tempfile::tempfile().unwrap();
    document.borrow().write_to(&mut saved).unwrap();
    saved.seek(SeekFrom::Start(0)).unwrap();
    let mut marker = [0; 2];
    saved.read_exact(&mut marker).unwrap();
    assert_eq!(marker, *b"**");
    let mut scratch = [0; 4096];
    for _ in 0..256 {
        saved.read_exact(&mut scratch).unwrap();
        assert_eq!(scratch, block);
    }
    saved.read_exact(&mut marker).unwrap();
    assert_eq!(marker, *b"**");
    assert_eq!(saved.read(&mut scratch).unwrap(), 0);
    drop(saved);
    let undo = owner.capture(1, &|_| true).unwrap();
    let undo = committed(
        owner
            .history(&undo, NativeMarkdownPagedInputHistory::Undo, &|_| true)
            .unwrap(),
    );
    assert_eq!(document.borrow().byte_len(), 1024 * 1024);
    assert_eq!(undo.state.selection, before);
    assert!(!document.borrow().can_undo());
    assert!(mapping.peak_managed_resident_bytes() <= 64 * 1024);
    drop(document);
    assert!(weak.upgrade().is_none());
    assert_eq!(mapping.mapped_bytes(), 0);
    assert!(mapping.unmap_succeeded() && !mapping.failed());
    assert!(commit.check_current(&|_| true).is_err()); // 原Input、已消费票与收据均不保活Doc或其映射。
}
