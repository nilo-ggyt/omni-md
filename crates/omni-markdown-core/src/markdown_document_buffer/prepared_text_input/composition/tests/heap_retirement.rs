//! Exercise heap append, bounded COW, and real history in one document; snapshots retain original Arcs and failures use the original checkpoint.

use super::*;

/// Read one finite fragment of a long synthetic document; the expected string describes only that checked position.
fn assert_span(document: &MarkdownDocumentBuffer, position: usize, expected: &str) {
    let units: Vec<_> = expected.encode_utf16().collect();
    assert!(units.len() <= 16);
    let mut actual = [0_u16; 16];
    document
        .copy_utf16_code_units(position, &mut actual[..units.len()])
        .unwrap();
    assert_eq!(&actual[..units.len()], units);
}

/// Check old snapshots through finite fragments too, without introducing a whole-document product read for validation.
fn assert_snapshot_span(snapshot: &MarkdownTextSnapshot, position: usize, expected: &str) {
    let units: Vec<_> = expected.encode_utf16().collect();
    assert!(units.len() <= 16);
    let mut actual = [0_u16; 16];
    snapshot
        .copy_utf16_code_units(position, &mut actual[..units.len()])
        .unwrap();
    assert_eq!(&actual[..units.len()], units);
}

#[test]
/// Repeated full updates keep actual heap storage/descriptors bounded while earlier ordinary input retains its independent Undo.
fn repeated_full_updates_reuse_two_heap_slots_and_keep_the_pre_composition_prefix_and_history() {
    let (_directory, mut document) = fixture("首尾", 4 * 1024 * 1024);
    document
        .replace_utf16_range(1..1, "前🙂", MarkdownTextSelection::caret(4))
        .unwrap();
    let baseline = document.text_snapshot();
    let mut previous = 0;
    let mut used = "前🙂".len();
    for index in 0..12 {
        let length = [8 * 1024, 60 * 1024, 16, EDIT_SEGMENT_BYTES][index % 4];
        let character = ['a', 'b', 'c', 'd'][index % 4];
        replace(
            &mut document,
            4..4 + previous,
            &character.to_string().repeat(length),
            MarkdownTextSelection::caret(4 + length),
            if index == 0 {
                Start
            } else {
                Update {
                    marked_before: 4..4 + previous,
                }
            },
        );
        used += length;
        assert_eq!(document.edits.used_bytes, used);
        assert!(document.edits.allocated_bytes() <= 2 * EDIT_SEGMENT_BYTES);
        assert!(document.edits.segments.len() <= 2);
        assert_span(&document, 0, "首前🙂");
        assert_span(&document, 4, &character.to_string());
        assert_span(&document, 3 + length, &format!("{character}尾"));
        previous = length;
    } // Cumulative input far exceeds 64 KiB, but current capacity/indices still cover only the original prefix and one candidate in at most two segments.
    replace(
        &mut document,
        4..4 + previous,
        "定🙂",
        MarkdownTextSelection::caret(7),
        Commit {
            marked_before: 4..4 + previous,
        },
    );
    assert_eq!(document.edits.used_bytes, used + "定🙂".len());
    assert_eq!(body(&document), "首前🙂定🙂尾".as_bytes());
    document.undo_text_edit().unwrap();
    assert_eq!(body(&document), "首前🙂尾".as_bytes());
    document.undo_text_edit().unwrap();
    assert_eq!(body(&document), "首尾".as_bytes());
    assert!(!document.can_undo());
    document.redo_text_edit().unwrap();
    document.redo_text_edit().unwrap();
    assert_eq!(body(&document), "首前🙂定🙂尾".as_bytes());
    assert_eq!(document.selection(), MarkdownTextSelection::caret(7));
    assert_snapshot_span(&baseline, 0, "首前🙂尾");
}

#[test]
/// A two-byte tail cannot split emoji; COW preserves baseline prefix and old two-segment snapshots through Undo/Redo.
fn scalar_aligned_two_segment_cow_keeps_the_original_prefix_and_existing_snapshot_bytes() {
    let (_directory, mut document) = fixture("首尾", 2 * 1024 * 1024);
    let prefix = EDIT_SEGMENT_BYTES - 2;
    let start = 1 + prefix;
    document
        .replace_utf16_range(
            1..1,
            &"p".repeat(prefix),
            MarkdownTextSelection::caret(start),
        )
        .unwrap();
    replace(
        &mut document,
        start..start,
        "a🙂中",
        MarkdownTextSelection::caret(start + 4),
        Start,
    );
    let snapshot = document.text_snapshot();
    replace(
        &mut document,
        start..start + 4,
        "😀文",
        MarkdownTextSelection::caret(start + 3),
        Update {
            marked_before: start..start + 4,
        },
    );
    assert_span(&document, start - 2, "pp😀文尾");
    assert_snapshot_span(&snapshot, start - 2, "ppa🙂中尾");
    replace(
        &mut document,
        start..start + 3,
        "终",
        MarkdownTextSelection::caret(start + 1),
        Commit {
            marked_before: start..start + 3,
        },
    );
    assert_span(&document, start - 2, "pp终尾");
    assert_eq!(document.edits.segments.len(), 2);
    assert!(document.edits.allocated_bytes() <= 2 * EDIT_SEGMENT_BYTES);
    document.undo_text_edit().unwrap();
    assert_eq!(document.utf16_len(), prefix + 2);
    assert_span(&document, start - 2, "pp尾");
    document.undo_text_edit().unwrap();
    assert_eq!(body(&document), "首尾".as_bytes());
    document.redo_text_edit().unwrap();
    document.redo_text_edit().unwrap();
    assert_span(&document, start - 2, "pp终尾");
    drop(document);
    assert_snapshot_span(&snapshot, start - 2, "ppa🙂中尾");
}

struct CandidateLine {
    reject: bool,
}

impl MarkdownTextChangeObserver for CandidateLine {
    type Error = &'static str;
    type Output = usize;

    fn changed(&mut self, _: changes::MarkdownTextChange) -> Result<(), Self::Error> {
        Ok(()) // The original transaction records real changes; this observer prepares only the actual candidate line needed by folding.
    }

    fn finish(
        self,
        candidate: changes::MarkdownPreparedText<'_>,
        after: MarkdownTextSelection,
    ) -> Result<usize, Self::Error> {
        let line = candidate.line_for_utf16(after.head_utf16).unwrap();
        if self.reject {
            return Err("真实候选观察者拒绝"); // Rejection after actual candidate-coordinate reads still must not install COW bytes.
        }
        Ok(line)
    }
}

#[test]
/// Observers read original appended candidates; rejection, explicit cancellation, and dropped-ticket draining restore segments before successful COW publication.
fn heap_cow_observer_rejection_and_both_cancellation_paths_restore_the_original_append_checkpoint()
{
    let (_directory, mut document) = fixture("首尾", 2 * 1024 * 1024);
    document
        .replace_utf16_range(1..1, "前\n", MarkdownTextSelection::caret(3))
        .unwrap();
    let length = 48 * 1024;
    replace(
        &mut document,
        3..3,
        &"a".repeat(length),
        MarkdownTextSelection::caret(3 + length),
        Start,
    );
    let root = document.root.clone();
    let profile = document.storage_profile();
    let before = document.selection();
    let candidate = "中🙂\n".repeat(8000);
    let after = MarkdownTextSelection::caret(3 + 4 * 8000);
    let prepare = |document: &mut MarkdownDocumentBuffer, reject| {
        document.prepare_composition_text_input_observed(
            3..3 + length,
            &candidate,
            before,
            after,
            Update {
                marked_before: 3..3 + length,
            },
            CandidateLine { reject },
        )
    };
    assert!(matches!(
        prepare(&mut document, true),
        Err(MarkdownObservedEditError::Observer("真实候选观察者拒绝"))
    ));
    assert_eq!(document.storage_profile(), profile);
    let (token, line) = prepare(&mut document, false).unwrap();
    assert_eq!(line, 8001);
    assert!(piece_tree_links_share_root(&document.root, &root));
    assert_span(&document, 0, "首前\naaa");
    document.cancel_prepared_text_input(&token).unwrap();
    assert_eq!(document.storage_profile(), profile);
    let (token, _) = prepare(&mut document, false).unwrap();
    drop(token);
    assert!(document.drain_cancelled_text_input());
    assert_eq!(document.storage_profile(), profile);
    assert_span(&document, 3 + length - 1, "a尾");
    let (token, line) = prepare(&mut document, false).unwrap();
    document.publish_text_input(&token).unwrap();
    assert_eq!(
        document
            .line_for_byte(document.byte_offset_for_utf16(after.head_utf16).unwrap())
            .unwrap(),
        line
    );
    assert_span(&document, 3, "中🙂\n");
    assert_span(&document, after.head_utf16 - 4, "中🙂\n尾");
    document.finish_text_input_composition().unwrap();
    document.undo_text_edit().unwrap();
    assert_eq!(body(&document), "首前\n尾".as_bytes());
    document.redo_text_edit().unwrap();
    assert_span(&document, 3, "中🙂\n");
    assert_span(&document, after.head_utf16 - 4, "中🙂\n尾");
}

#[test]
/// Full/partial/full updates rebase only the latest append; retained candidate sides and old snapshots cannot be retired or rewritten.
fn partial_then_full_update_keeps_the_older_candidate_fragments_and_one_net_history() {
    let (_directory, mut document) = fixture("首尾", 1024 * 1024);
    replace(
        &mut document,
        1..1,
        "ABCDE🙂FG",
        MarkdownTextSelection::caret(10),
        Start,
    );
    replace(
        &mut document,
        2..4,
        "中🙂",
        MarkdownTextSelection::caret(5),
        Update {
            marked_before: 1..10,
        },
    );
    let snapshot = document.text_snapshot();
    replace(
        &mut document,
        2..5,
        "好😀",
        MarkdownTextSelection::caret(5),
        Commit {
            marked_before: 2..5,
        },
    );
    assert_eq!(body(&document), "首A好😀DE🙂FG尾".as_bytes());
    assert_snapshot_span(&snapshot, 0, "首A中🙂DE🙂FG尾");
    document.undo_text_edit().unwrap();
    assert_eq!(body(&document), "首尾".as_bytes());
    document.redo_text_edit().unwrap();
    assert_eq!(body(&document), "首A好😀DE🙂FG尾".as_bytes());
    assert_snapshot_span(&snapshot, 0, "首A中🙂DE🙂FG尾");
    assert_eq!(document.edits.used_bytes, "ABCDE🙂FG中🙂好😀".len());
}

#[test]
/// Heap/Frozen transitions retire each actual storage payload; long candidates stay file-backed while old snapshots own their original finite heap segments.
fn heap_to_frozen_to_heap_retires_each_actual_payload_without_truncating_the_new_frozen_slot() {
    let (_directory, mut document) = fixture("首尾", 2 * 1024 * 1024);
    let small = 8 * 1024;
    replace(
        &mut document,
        1..1,
        &"a".repeat(small),
        MarkdownTextSelection::caret(1 + small),
        Start,
    );
    let snapshot = document.text_snapshot();
    let large = EDIT_SEGMENT_BYTES + 3;
    replace(
        &mut document,
        1..1 + small,
        &"b".repeat(large),
        MarkdownTextSelection::caret(1 + large),
        Update {
            marked_before: 1..1 + small,
        },
    );
    assert_eq!(document.edits.allocated_bytes(), 0);
    let observer = document.backing_observers().last().unwrap();
    assert_eq!(observer.mapped_bytes(), large);
    assert_span(&document, 1, "bbb");
    assert_span(&document, large - 1, "bb尾");
    replace(
        &mut document,
        1..1 + large,
        "中文🙂",
        MarkdownTextSelection::caret(5),
        Commit {
            marked_before: 1..1 + large,
        },
    );
    assert_eq!(body(&document), "首中文🙂尾".as_bytes());
    assert_eq!(observer.mapped_bytes(), 0);
    assert!(observer.unmap_succeeded() && !observer.failed());
    assert_snapshot_span(&snapshot, 1, "aaa");
    assert_snapshot_span(&snapshot, small - 1, "aa尾");
    document.undo_text_edit().unwrap();
    assert_eq!(body(&document), "首尾".as_bytes());
    document.redo_text_edit().unwrap();
    assert_eq!(body(&document), "首中文🙂尾".as_bytes());
}

#[test]
/// Replacing/deleting physical candidates does not refund cumulative session append bytes; budget failure preserves the original root and heap.
fn reclaiming_heap_candidates_does_not_refund_the_cumulative_session_edit_budget() {
    let length = 32 * 1024;
    let (_directory, mut document) = fixture("首尾", (length * 3) as u64);
    for index in 0..3 {
        replace(
            &mut document,
            if index == 0 { 1..1 } else { 1..1 + length },
            &"a".repeat(length),
            MarkdownTextSelection::caret(1 + length),
            if index == 0 {
                Start
            } else {
                Update {
                    marked_before: 1..1 + length,
                }
            },
        );
    }
    assert_eq!(document.edits.used_bytes, length * 3);
    assert_eq!(document.edits.segments.len(), 1);
    let root = document.root.clone();
    let profile = document.storage_profile();
    let before = document.selection();
    assert!(matches!(
        document.prepare_composition_text_input_observed(
            1..1 + length,
            &"b".repeat(length),
            before,
            before,
            Update {
                marked_before: 1..1 + length
            },
            Observe::default(),
        ),
        Err(MarkdownObservedEditError::Document(
            MarkdownDocumentBufferError::EditBufferLimitExceeded
        ))
    ));
    assert!(piece_tree_links_share_root(&document.root, &root));
    assert_eq!(document.storage_profile(), profile);
    replace(
        &mut document,
        1..1 + length,
        "",
        MarkdownTextSelection::caret(1),
        Commit {
            marked_before: 1..1 + length,
        },
    );
    assert_eq!(body(&document), "首尾".as_bytes());
    assert_eq!(document.edits.allocated_bytes(), 0);
    assert_eq!(document.edits.used_bytes, length * 3);
    assert!(matches!(
        document.prepare_text_input_observed(
            1..1,
            "x",
            document.selection(),
            MarkdownTextSelection::caret(2),
            Observe::default(),
        ),
        Err(MarkdownObservedEditError::Document(
            MarkdownDocumentBufferError::EditBufferLimitExceeded
        ))
    ));
}

#[test]
/// Source reclaim failure after COW preparation prevents byte installation; explicit cancellation rolls back original append while retaining EIO.
fn original_source_failure_before_publish_does_not_install_the_prepared_heap_cow() {
    let (_directory, mut document) = fixture("首尾", 1024 * 1024);
    let length = 8 * 1024;
    replace(
        &mut document,
        1..1,
        &"a".repeat(length),
        MarkdownTextSelection::caret(1 + length),
        Start,
    );
    let root = document.root.clone();
    let profile = document.storage_profile();
    let before = document.selection();
    let (token, _) = document
        .prepare_composition_text_input_observed(
            1..1 + length,
            &"b".repeat(length),
            before,
            before,
            Update {
                marked_before: 1..1 + length,
            },
            Observe::default(),
        )
        .unwrap();
    let observer = document.original_backing_observer().unwrap();
    observer.fail_next_reclaim_for_test();
    let error = MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO);
    assert_eq!(
        document.reclaim_original_read_windows_for_test(),
        Err(error)
    );
    assert_eq!(document.publish_text_input(&token), Err(error));
    assert!(document.has_prepared_text_input());
    document.cancel_prepared_text_input(&token).unwrap();
    assert!(piece_tree_links_share_root(&document.root, &root));
    assert_eq!(document.storage_profile(), profile);
    assert_eq!(document.selection(), before);
    assert_eq!(&document.edits.segments[0].as_slice()[..4], b"aaaa"); // Inspect only this actual heap prefix; do not read Source through the failed document.
    assert_eq!(observer.first_errno(), Some(libc::EIO));
    drop(document);
    assert!(observer.unmap_succeeded());
    assert_eq!(observer.mapped_bytes(), 0);
    assert!(observer.failed());
}
