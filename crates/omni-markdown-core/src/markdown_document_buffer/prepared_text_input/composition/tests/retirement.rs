//! 真实文件候选的退休只结束Doc强边；快照、净历史与最终映射释放证据各自验证。

use super::*;

/// 大样本只比较有限首尾，不创建整篇预期String或读取整篇canonical。
fn assert_edges(snapshot: &MarkdownTextSnapshot, length: usize, first: char, last: char) {
    assert_eq!(snapshot.utf16_len(), length);
    let mut unit = [0_u16; 1];
    snapshot.copy_utf16_code_units(1, &mut unit).unwrap();
    assert_eq!(unit[0], first as u16);
    snapshot
        .copy_utf16_code_units(length - 2, &mut unit)
        .unwrap();
    assert_eq!(unit[0], last as u16);
}

/// 票来自本次真实发布的最后文件段；原文件始终是迭代器首项，不冒充编辑映射。
fn latest_backing(document: &MarkdownDocumentBuffer) -> crate::MarkdownOriginalBackingObserver {
    document.backing_observers().last().unwrap()
}

#[test]
/// 多次完整候选替换在Doc仍活时真实归还旧VM，最终确认仍只有一次组合Undo/Redo。
fn full_frozen_updates_retire_superseded_payloads_without_losing_the_first_history_root() {
    let (_directory, mut document) = fixture("首尾", 2 * 1024 * 1024);
    document
        .set_selection(MarkdownTextSelection::caret(1))
        .unwrap();
    let length = EDIT_SEGMENT_BYTES + 17;
    let mut observers: Vec<crate::MarkdownOriginalBackingObserver> = Vec::new();
    for character in ['a', 'b', 'c'] {
        let range = if observers.is_empty() {
            1..1
        } else {
            1..1 + length
        };
        let composition = if observers.is_empty() {
            Start
        } else {
            Update {
                marked_before: 1..1 + length,
            }
        };
        replace(
            &mut document,
            range,
            &character.to_string().repeat(length),
            MarkdownTextSelection::caret(1 + length),
            composition,
        );
        for observer in &observers {
            assert_eq!(observer.mapped_bytes(), 0);
            assert!(observer.unmap_succeeded() && !observer.failed());
        } // 下一次候选已发布时，全部被完整替换的旧payload都必须真实归还。
        observers.push(latest_backing(&document));
        assert_edges(&document.text_snapshot(), length + 2, character, character);
    }
    replace(
        &mut document,
        1..1 + length,
        "终🙂",
        MarkdownTextSelection::caret(4),
        Commit {
            marked_before: 1..1 + length,
        },
    );
    for observer in &observers {
        assert_eq!(observer.mapped_bytes(), 0);
        assert!(observer.unmap_succeeded() && !observer.failed());
    }
    assert_eq!(document.edits.used_bytes, length * 3 + "终🙂".len());
    assert_eq!(document.backing_observers().count(), observers.len() + 1);
    assert_eq!(body(&document), "首终🙂尾".as_bytes());
    document.undo_text_edit().unwrap();
    assert_eq!(body(&document), "首尾".as_bytes());
    assert!(!document.can_undo());
    document.redo_text_edit().unwrap();
    assert_eq!(body(&document), "首终🙂尾".as_bytes());
    assert_eq!(document.selection(), MarkdownTextSelection::caret(4));
}

#[test]
/// 独立快照继续读原候选；退休票必须等最后实际快照Drop才观察到munmap。
fn retired_frozen_slot_preserves_its_observer_and_an_existing_snapshot_keeps_exact_old_bytes() {
    let (_directory, mut document) = fixture("首尾", 1024 * 1024);
    let length = EDIT_SEGMENT_BYTES + 7;
    replace(
        &mut document,
        1..1,
        &"x".repeat(length),
        MarkdownTextSelection::caret(1 + length),
        Start,
    );
    let snapshot = document.text_snapshot();
    let observer = latest_backing(&document);
    replace(
        &mut document,
        1..1 + length,
        "已确认",
        MarkdownTextSelection::caret(4),
        Commit {
            marked_before: 1..1 + length,
        },
    );
    assert!(matches!(
        document.edits.segments[0].as_ref(),
        MarkdownEditSegment::RetiredFrozen(_)
    ));
    assert_eq!(observer.mapped_bytes(), length);
    assert!(!observer.unmap_succeeded());
    assert_edges(&snapshot, length + 2, 'x', 'x');
    assert_eq!(body(&document), "首已确认尾".as_bytes());
    drop(snapshot);
    assert_eq!(observer.mapped_bytes(), 0);
    assert!(observer.unmap_succeeded() && !observer.failed());
    assert_eq!(document.backing_observers().count(), 2); // 已归还票仍在原published集合，不因退休消失。
}

#[test]
/// 部分marked替换留下原文件的真实两侧，随后完整替换新小候选也不能误退这份文件。
fn partial_updates_keep_the_original_frozen_payload_needed_by_the_canonical_prefix_and_suffix() {
    let (_directory, mut document) = fixture("首尾", 1024 * 1024);
    let length = EDIT_SEGMENT_BYTES + 11;
    replace(
        &mut document,
        1..1,
        &"a".repeat(length),
        MarkdownTextSelection::caret(1 + length),
        Start,
    );
    let observer = latest_backing(&document);
    replace(
        &mut document,
        10..11,
        "中",
        MarkdownTextSelection::caret(11),
        Update {
            marked_before: 1..1 + length,
        },
    );
    replace(
        &mut document,
        10..11,
        "文",
        MarkdownTextSelection::caret(11),
        Commit {
            marked_before: 10..11,
        },
    );
    assert!(matches!(
        document.edits.segments[0].as_ref(),
        MarkdownEditSegment::Frozen(_)
    ));
    assert_eq!(observer.mapped_bytes(), length);
    let snapshot = document.text_snapshot();
    assert_edges(&snapshot, length + 2, 'a', 'a');
    let mut unit = [0_u16; 1];
    snapshot.copy_utf16_code_units(10, &mut unit).unwrap();
    assert_eq!(unit[0], '文' as u16);
    drop(snapshot);
    document.undo_text_edit().unwrap();
    assert_eq!(body(&document), "首尾".as_bytes());
    document.redo_text_edit().unwrap();
    assert_eq!(observer.mapped_bytes(), length); // Redo根仍读取旧候选两侧，真实payload必须保留。
    drop(document);
    assert_eq!(observer.mapped_bytes(), 0);
    assert!(observer.unmap_succeeded() && !observer.failed());
}

#[test]
/// 新client或已确认后的Start有独立Undo基线；即使替换同范围，也必须保留前组文件供历史恢复。
fn a_new_composition_start_keeps_the_prior_frozen_payload_for_its_independent_undo_root() {
    let (_directory, mut document) = fixture("首尾", 1024 * 1024);
    let length = EDIT_SEGMENT_BYTES + 31;
    replace(
        &mut document,
        1..1,
        &"p".repeat(length),
        MarkdownTextSelection::caret(1 + length),
        Start,
    );
    let observer = latest_backing(&document);
    replace(
        &mut document,
        1..1 + length,
        "新组",
        MarkdownTextSelection::caret(3),
        Start,
    ); // 不调用finish也覆盖旧client只close本地Input、同Doc残留marked元数据的真实边界。
    document.finish_text_input_composition().unwrap();
    assert_eq!(observer.mapped_bytes(), length);
    assert!(matches!(
        document.edits.segments[0].as_ref(),
        MarkdownEditSegment::Frozen(_)
    ));
    document.undo_text_edit().unwrap();
    assert_edges(&document.text_snapshot(), length + 2, 'p', 'p');
    document.undo_text_edit().unwrap();
    assert_eq!(body(&document), "首尾".as_bytes());
    document.redo_text_edit().unwrap();
    assert_edges(&document.text_snapshot(), length + 2, 'p', 'p');
    document.redo_text_edit().unwrap();
    assert_eq!(body(&document), "首新组尾".as_bytes());
    assert_eq!(observer.mapped_bytes(), length);
    drop(document);
    assert_eq!(observer.mapped_bytes(), 0);
    assert!(observer.unmap_succeeded() && !observer.failed());
}

#[test]
/// 取消、预算与观察者失败不消费旧extent票；取消的新Frozen文件必须实际归还。
fn rejected_or_cancelled_frozen_update_keeps_the_original_payload_and_retirement_authority() {
    let (_directory, mut document) = fixture("首尾", 512 * 1024);
    let length = EDIT_SEGMENT_BYTES + 13;
    replace(
        &mut document,
        1..1,
        &"a".repeat(length),
        MarkdownTextSelection::caret(1 + length),
        Start,
    );
    let observer = latest_backing(&document);
    let root = document.root.clone();
    let profile = document.storage_profile();
    let before = document.selection();
    let prepare = |document: &mut MarkdownDocumentBuffer, reject| {
        document.prepare_composition_text_input_observed(
            1..1 + length,
            &"b".repeat(length),
            before,
            before,
            Update {
                marked_before: 1..1 + length,
            },
            Observe {
                reject,
                ..Observe::default()
            },
        )
    };
    assert!(matches!(
        prepare(&mut document, true),
        Err(MarkdownObservedEditError::Observer(_))
    ));
    assert_eq!(document.storage_profile(), profile);
    let (token, _) = prepare(&mut document, false).unwrap();
    let MarkdownEditSegment::Frozen(backing) = document.edits.segments.last().unwrap().as_ref()
    else {
        panic!("本次真实候选应独占新Frozen文件");
    };
    let pending_observer = backing.observer();
    document.cancel_prepared_text_input(&token).unwrap();
    assert_eq!(pending_observer.mapped_bytes(), 0);
    assert!(pending_observer.unmap_succeeded() && !pending_observer.failed());
    assert_eq!(document.storage_profile(), profile);
    assert!(piece_tree_links_share_root(&document.root, &root));
    assert_eq!(observer.mapped_bytes(), length);
    assert!(!observer.unmap_succeeded());
    assert!(
        document
            .prepare_composition_text_input_observed(
                1..1 + length,
                &"超".repeat(512 * 1024),
                before,
                MarkdownTextSelection::caret(1),
                Update {
                    marked_before: 1..1 + length
                },
                Observe::default(),
            )
            .is_err()
    );
    assert_eq!(document.storage_profile(), profile);
    replace(
        &mut document,
        1..1 + length,
        "确认",
        MarkdownTextSelection::caret(3),
        Commit {
            marked_before: 1..1 + length,
        },
    );
    assert_eq!(observer.mapped_bytes(), 0);
    assert!(observer.unmap_succeeded() && !observer.failed());
    document.undo_text_edit().unwrap();
    assert_eq!(body(&document), "首尾".as_bytes());
}

#[test]
/// 发布后真实旧payload msync故障不冒充未提交；同编号保EIO和munmap的独立结果。
fn retirement_drop_failure_keeps_committed_canonical_and_the_original_published_errno() {
    let (_directory, mut document) = fixture("首尾", 1024 * 1024);
    let length = EDIT_SEGMENT_BYTES + 19;
    replace(
        &mut document,
        1..1,
        &"a".repeat(length),
        MarkdownTextSelection::caret(1 + length),
        Start,
    );
    let observer = latest_backing(&document);
    let attempts = observer.reclaim_attempts();
    let generation = document.generation();
    let before = document.selection();
    let (token, _) = document
        .prepare_composition_text_input_observed(
            1..1 + length,
            "确认🙂",
            before,
            MarkdownTextSelection::caret(5),
            Commit {
                marked_before: 1..1 + length,
            },
            Observe::default(),
        )
        .unwrap();
    observer.fail_next_reclaim_for_test(); // 仅在成功prepare后武装；首次真实失败必须来自旧payload Drop。
    let edit = document.publish_text_input(&token).unwrap();
    assert_eq!(edit.replaced_utf16_range, 1..1 + length);
    assert_eq!(document.generation(), generation + 1);
    assert_eq!(document.selection(), MarkdownTextSelection::caret(5));
    assert_eq!(document.utf16_len(), 6);
    assert!(observer.reclaim_attempts() > attempts);
    assert_eq!(observer.first_errno(), Some(libc::EIO));
    assert!(observer.unmap_succeeded());
    assert_eq!(observer.mapped_bytes(), 0);
    assert_eq!(
        document.original_backing_health(),
        Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
            libc::EIO
        ))
    );
    let mut units = [0_u16; 4];
    document
        .text_snapshot()
        .copy_utf16_code_units(1, &mut units)
        .unwrap();
    assert_eq!(String::from_utf16(&units).unwrap(), "确认🙂"); // 只核已提交的有限片段，不绕过Doc故障继续输入。
    let final_observers: Vec<_> = document.backing_observers().collect();
    assert_eq!(final_observers[1].first_errno(), Some(libc::EIO));
    drop(document);
    assert!(
        final_observers
            .iter()
            .all(|observer| observer.unmap_succeeded() && observer.mapped_bytes() == 0)
    );
    assert!(final_observers[1].failed()); // 最终真实资源观察仍保负面事实，不把munmap成功洗成全部成功。
}

#[test]
/// 退休之后合法旧Snapshot仍拥有文件；其末次真实Drop失败必须继续撤销原Doc后继权限。
fn delayed_snapshot_drop_failure_stays_visible_in_the_same_retired_slot() {
    let (_directory, mut document) = fixture("首尾", 1024 * 1024);
    let length = EDIT_SEGMENT_BYTES + 29;
    replace(
        &mut document,
        1..1,
        &"s".repeat(length),
        MarkdownTextSelection::caret(1 + length),
        Start,
    );
    let snapshot = document.text_snapshot();
    let observer = latest_backing(&document);
    replace(
        &mut document,
        1..1 + length,
        "定稿",
        MarkdownTextSelection::caret(3),
        Commit {
            marked_before: 1..1 + length,
        },
    );
    assert_eq!(document.original_backing_health(), Ok(()));
    assert_eq!(observer.mapped_bytes(), length);
    let attempts = observer.reclaim_attempts();
    observer.fail_next_reclaim_for_test(); // 原Doc已无强边；只由真实最后Snapshot清理消费此注入。
    drop(snapshot);
    assert!(observer.reclaim_attempts() > attempts);
    assert_eq!(observer.first_errno(), Some(libc::EIO));
    assert_eq!(observer.mapped_bytes(), 0);
    assert!(observer.unmap_succeeded());
    assert_eq!(
        document.original_backing_health(),
        Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
            libc::EIO
        ))
    );
    let original = document.root.clone();
    assert!(matches!(
        document.prepare_text_input_observed(
            1..1,
            "不能在失败源继续发布",
            document.selection(),
            MarkdownTextSelection::caret(1),
            Observe::default(),
        ),
        Err(MarkdownObservedEditError::Document(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
    assert!(piece_tree_links_share_root(&document.root, &original));
    assert_eq!(
        document.backing_observers().nth(1).unwrap().first_errno(),
        Some(libc::EIO)
    );
}
