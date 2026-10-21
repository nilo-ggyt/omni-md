//! Exercise Input formatting through Core transactions, checking actual Unicode selection/history, rollback, and shared mappings.

use super::*;
use omni_markdown_core::MarkdownFormatAction as Format;

/// Inline inner/block selections follow their Core rules; one actual Undo restores the original reversed selection.
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
        }; // Every reentrant authority callback runs outside document/fold borrows.
        let original = owner.state().unwrap();
        let permit = owner.capture(1, &current).unwrap();
        let commit = owner.apply_format(&permit, action, &current).unwrap();
        assert_eq!(text(&document), expected);
        assert_eq!(commit.state.generation, original.generation + 1);
        // Inline wrapping selects the inner text forward as Core defines; Native must not alter direction afterward and corrupt history after.
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
        assert_eq!(undo.state.selection, before); // The sole Undo restores the actual reversed selection before the menu action.
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
        assert_eq!(commit.state.selection, after); // Block formatting retains direction, including the actual reversed after-selection in this adapter.
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

/// An empty caret uses the existing placeholder/template selection directly, without another insertion followed by selection correction.
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
        assert!(!document.borrow().can_undo()); // An empty-caret template still creates just one canonical formatting transaction.
    }
}

/// Actual endpoint insertions retain interior folds; scalar rejection by the candidate observer rolls back text and history together.
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
    // Only endpoint insertions occurred; using a replacement envelope would incorrectly delete the interior fold instead of shifting it.
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
    // The mapper checks integer ranges; the real candidate observer must also reject old fold endpoints inside a surrogate pair.
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
    assert!(!owner.is_busy()); // Observer rejection returns both the added-marker candidate and Mutation admission.
}

/// Formatting preflight preserves IME/multiple selection/budgets without manufacturing a commit from the primary memento or newer authority.
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
    ); // Small formatting markers do not relax the configured live-byte limit.
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

/// Revoking a committed receipt's display authority changes no canonical facts and never authorizes old-permit replay.
#[test]
fn source_format_committed_receipt_revocation_does_not_replay_or_rollback() {
    let (_file, document) = fixture("甲", 4096);
    let owner = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
    let epoch = Cell::new(1);
    let current = |expected| expected == epoch.get();
    let permit = owner.capture(1, &current).unwrap();
    let commit = owner.apply_format(&permit, Format::Bold, &current).unwrap();
    epoch.set(2); // End original window authority after Core publication; the receipt stops effects without rolling back text.
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

/// Cell formatting publishes only its local root/history; marked input, block location, stale authority, and Source failure leave canonical text unchanged.
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
    } // Exceed the 64 KiB resident slots so bounded reads cause actual eviction rather than directly setting Source failure.
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
        ); // Verify every byte of the fixed short table without constructing a large canonical mirror.
        let mut offset = input.len();
        let mut actual = [0; 4096];
        while offset < original.utf16_len() {
            let count = actual.len().min(original.utf16_len() - offset);
            original
                .copy_utf16_code_units(offset, &mut actual[..count])
                .unwrap();
            assert!(actual[..count].iter().all(|unit| *unit == u16::from(b'x')));
            offset += count;
        } // Use fixed read windows to verify the entire tail remains unchanged by local formatting/history.
    };
    let verified = {
        let document = document.borrow();
        let mut request = document
            .table_window_request(
                MarkdownTableWindowTarget::AtUtf16(0),
                0..input.encode_utf16().count(), // Request only the short table; the tail drives actual page eviction and is outside the table viewport.
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
    assert_eq!(units, [0x62, 0x61, 0x73, 0x65]); // Rejected formatting adds no local Undo and commits no canonical text.
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
    }; // Inspect existing public canonical facts without adding storage counters for adapter tests.
    let baseline = canonical_state();
    let current = |_| {
        assert!(document.try_borrow_mut().is_ok());
        assert!(owner.folds.try_borrow_mut().is_ok());
        true
    }; // External authority callbacks run outside Core borrows; no callback after successful Cell publication may turn success into rejection.
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
    ); // Actual content_serial revokes the original Cell Source; an empty document Weak cannot masquerade as a Body edit.
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
    assert_eq!(redo.state.utf16_len, 9); // Caret movement cannot rewrite the isolated format group's actual after-selection.
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
    } // Report the location error without implicitly committing Cell or removing existing Body menu actions.
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
    )); // Original edit-serial failure precedes a new format-location check; recapturing the current caret cannot renew an old command.
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
        // The prior bounded oracle reached the tail; reading the first window again must evict an actual original page.
        // The observer injects synthetic EIO only after successful actual msync; this is not a kernel failure claim.
        assert!(matches!(
            owner.apply_format(&original, Format::Heading1, &|_| false),
            Err(NativeMarkdownPagedInputError::Cell(
                MarkdownCellDraftError::Document(
                    MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
                )
            ))
        )); // Source EIO precedes revocation/block-location checks; input rejection cannot become a generic menu failure.
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
    } // Compare the complete original file in chunks; Source failure/local cancellation must not rewrite persisted input.
}

/// A large selection adds bounded markers and shares original mapping; verify complete streamed save and actual resource release.
#[test]
fn source_format_large_surround_keeps_shared_mapping_and_exact_saved_bytes() {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = tempfile::NamedTempFile::new().unwrap();
    let block = [b'x'; 4096];
    for _ in 0..256 {
        file.write_all(&block).unwrap(); // Generate the fixture and inspect complete saved output in fixed 4 KiB blocks without a full-text String.
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
    assert!(commit.check_current(&|_| true).is_err()); // Input, consumed permits, and receipts retain neither document nor mapping.
}
