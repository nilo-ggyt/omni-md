//! 局部历史、状态版本和同Source布局桥的真实资源回归；不创建第二Doc或合成Ready。

use super::*;
#[cfg(feature = "paged-layout-breaks")]
use crate::{BidiBaseDirection, BidiMemoryBudget, MarkdownBidiParagraph, MarkdownLayoutTextSource};

fn restore(
    doc: &mut MarkdownDocumentBuffer,
    token: &MarkdownCellDraft,
    direction: MarkdownCellDraftHistoryDirection,
) -> Option<MarkdownCellDraftHistoryEdit> {
    let permit = doc.capture_cell_draft_edit(token).unwrap();
    doc.restore_cell_draft_history(permit, direction).unwrap()
}

#[test]
fn local_undo_redo_restores_raw_roots_without_publishing_canonical_history_or_append() {
    let (_dir, mut doc) = document(SOURCE);
    let before = doc.storage_profile();
    let token = begin(&mut doc);
    replace(&mut doc, &token, "中😀");
    replace(&mut doc, &token, "second");
    let second = doc.cell_draft_facts(&token).unwrap();
    let delta = doc
        .cell_draft_resource_observer(&token)
        .unwrap()
        .snapshot()
        .delta_bytes;
    let undo = restore(&mut doc, &token, MarkdownCellDraftHistoryDirection::Undo).unwrap();
    assert_eq!(undo.replaced_utf16_range, 0..6);
    assert_eq!(undo.change_in_utf16_length, -3);
    assert!(undo.facts.serial > second.serial && undo.facts.content_serial > second.content_serial);
    let capsule = doc.capture_cell_draft(&token).unwrap();
    assert_eq!(read(&capsule), "中😀");
    drop(capsule);
    restore(&mut doc, &token, MarkdownCellDraftHistoryDirection::Redo).unwrap();
    let capsule = doc.capture_cell_draft(&token).unwrap();
    assert_eq!(read(&capsule), "second");
    assert_eq!(doc.storage_profile(), before);
    assert_eq!(canonical(&doc), SOURCE);
    assert_eq!(
        doc.cell_draft_resource_observer(&token)
            .unwrap()
            .snapshot()
            .delta_bytes,
        delta
    );
}

#[test]
fn raw_history_has_exact_128_root_budget_and_new_input_retires_redo_without_refunding_delta() {
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    for _ in 0..140 {
        let at = doc.cell_draft_facts(&token).unwrap().utf16_len;
        edit(
            &mut doc,
            &token,
            at..at,
            "x",
            MarkdownTextSelection::caret(at + 1),
            None,
        )
        .unwrap();
    }
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let delta = observer.snapshot().delta_bytes;
    let mut count = 0;
    while restore(&mut doc, &token, MarkdownCellDraftHistoryDirection::Undo).is_some() {
        count += 1;
    }
    assert_eq!(count, 128);
    assert_eq!(doc.cell_draft_facts(&token).unwrap().utf16_len, 13);
    assert_eq!(observer.snapshot().delta_bytes, delta);
    replace(&mut doc, &token, "branch");
    assert!(restore(&mut doc, &token, MarkdownCellDraftHistoryDirection::Redo).is_none());
    assert!(observer.snapshot().delta_bytes > delta);
    assert_eq!(canonical(&doc), SOURCE);
}

#[test]
#[cfg(feature = "paged-layout-breaks")]
fn selection_and_unmark_revoke_input_capsule_but_keep_actual_same_content_source_and_cache() {
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    edit(
        &mut doc,
        &token,
        0..1,
        "中😀",
        MarkdownTextSelection::caret(3),
        Some(0..3),
    )
    .unwrap();
    let source = MarkdownLayoutTextSource::capture_cell_draft(&doc, &token).unwrap();
    let snapshot = source.snapshot_for_bidi();
    let source_authority = source.cell_draft_authority().unwrap();
    let before = doc.cell_draft_facts(&token).unwrap();
    let stale_edit = doc.capture_cell_draft_edit(&token).unwrap();
    let permit = doc.capture_cell_draft_edit(&token).unwrap();
    let unmarked = doc.unmark_cell_draft(permit).unwrap();
    assert!(unmarked.serial > before.serial);
    assert_eq!(unmarked.content_serial, before.content_serial);
    assert!(unmarked.marked_range.is_none());
    assert_eq!(
        doc.replace_cell_draft(
            stale_edit,
            0..0,
            "bad",
            MarkdownTextSelection::caret(3),
            None
        ),
        Err(MarkdownCellDraftError::Stale)
    );
    let permit = doc.capture_cell_draft_edit(&token).unwrap();
    let selected = doc
        .set_cell_draft_selection(
            permit,
            MarkdownTextSelection {
                anchor_utf16: 0,
                head_utf16: 3,
            },
        )
        .unwrap();
    assert_eq!(selected.content_serial, before.content_serial);
    assert!(source.cell_draft_authority().unwrap() == source_authority);
    assert_eq!(snapshot.byte_offset_for_utf16(3), Ok("中😀".len()));
    let mut units = [0; 3];
    snapshot.copy_utf16_code_units(0, &mut units).unwrap();
    assert_eq!(units, [0x4e2d, 0xd83d, 0xde00]);
    assert_eq!(canonical(&doc), SOURCE);
}

#[test]
fn marked_requires_real_unmark_before_history_and_undo_does_not_restart_composition() {
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    edit(
        &mut doc,
        &token,
        0..1,
        "中😀",
        MarkdownTextSelection::caret(3),
        Some(0..3),
    )
    .unwrap();
    let before = doc.cell_draft_facts(&token).unwrap();
    let permit = doc.capture_cell_draft_edit(&token).unwrap();
    assert_eq!(
        doc.restore_cell_draft_history(permit, MarkdownCellDraftHistoryDirection::Undo),
        Err(MarkdownCellDraftError::CompositionActive)
    );
    assert_eq!(doc.cell_draft_facts(&token).unwrap(), before);
    let permit = doc.capture_cell_draft_edit(&token).unwrap();
    doc.unmark_cell_draft(permit).unwrap();
    let restored = restore(&mut doc, &token, MarkdownCellDraftHistoryDirection::Undo).unwrap();
    assert_eq!(restored.facts.utf16_len, 1);
    assert!(restored.facts.marked_range.is_none());
    let redone = restore(&mut doc, &token, MarkdownCellDraftHistoryDirection::Redo).unwrap();
    assert_eq!(redone.facts.utf16_len, 3);
    assert!(redone.facts.marked_range.is_none());
}

#[test]
fn empty_input_changes_only_state_and_rejects_invalid_ranges_without_creating_history() {
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    let before = doc.cell_draft_facts(&token).unwrap();
    let capsule = doc.capture_cell_draft(&token).unwrap();
    let authority = capsule.authority();
    let updated = edit(
        &mut doc,
        &token,
        1..1,
        "",
        MarkdownTextSelection::caret(1),
        Some(0..1),
    )
    .unwrap();
    assert!(updated.serial > before.serial);
    assert_eq!(updated.content_serial, before.content_serial);
    assert_eq!(read(&capsule), "a");
    assert_eq!(capsule.facts(&authority).unwrap(), before);
    let unchanged = edit(
        &mut doc,
        &token,
        1..1,
        "",
        MarkdownTextSelection::caret(1),
        Some(0..1),
    )
    .unwrap();
    assert_eq!(unchanged, updated);
    assert_eq!(
        edit(
            &mut doc,
            &token,
            2..2,
            "",
            MarkdownTextSelection::caret(1),
            None,
        ),
        Err(MarkdownCellDraftError::Document(
            MarkdownDocumentBufferError::RangeOutsideDocument
        ))
    );
    assert_eq!(doc.cell_draft_facts(&token).unwrap(), updated);
    let permit = doc.capture_cell_draft_edit(&token).unwrap();
    doc.unmark_cell_draft(permit).unwrap();
    assert!(restore(&mut doc, &token, MarkdownCellDraftHistoryDirection::Undo).is_none());
    assert_eq!(
        doc.cell_draft_resource_observer(&token)
            .unwrap()
            .snapshot()
            .delta_bytes,
        0
    );
}

#[test]
#[cfg(feature = "paged-layout-breaks")]
fn draft_layout_source_has_local_domain_and_internal_bidi_snapshot_cannot_outlive_its_guard() {
    let (_dir, mut doc) = document(SOURCE);
    let canonical_source = MarkdownLayoutTextSource::capture(&doc);
    assert!(canonical_source.cell_draft_authority().is_none());
    let token = begin(&mut doc);
    replace(&mut doc, &token, "abc אבג");
    let source = MarkdownLayoutTextSource::capture_cell_draft(&doc, &token).unwrap();
    let authority = source.authority();
    let snapshot = source.snapshot_for_bidi();
    assert_eq!(source.utf16_len(), 7);
    assert!(source.cell_draft_authority().is_some());
    assert!(authority != canonical_source.authority());
    let bidi = MarkdownBidiParagraph::analyze_layout_source(
        source.clone(),
        &authority,
        0..source.utf16_len(),
        BidiBaseDirection::Auto,
        BidiMemoryBudget::default(),
    )
    .unwrap();
    bidi.require_layout_source(&authority).unwrap();
    replace(&mut doc, &token, "new");
    assert_eq!(
        snapshot.byte_offset_for_utf16(0),
        Err(MarkdownDocumentBufferError::PreparedTextInputStale)
    );
    assert!(matches!(
        bidi.require_layout_source(&authority),
        Err(crate::BidiError::Source(
            MarkdownDocumentBufferError::PreparedTextInputStale
        ))
    ));
    assert_eq!(canonical(&doc), SOURCE);
}

#[test]
#[cfg(feature = "paged-layout-breaks")]
fn internal_snapshot_clones_keep_actual_read_claim_until_last_payload_and_source_really_unmaps() {
    let (_dir, mut doc) = document(SOURCE);
    let backing = doc.original_backing_observer().unwrap();
    let token = begin(&mut doc);
    replace(&mut doc, &token, "only edited bytes");
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let source = MarkdownLayoutTextSource::capture_cell_draft(&doc, &token).unwrap();
    assert!(source.original_backing_observer().is_some());
    let raw_clone = source.snapshot_for_bidi();
    assert_eq!(observer.snapshot().read_roots, 1);
    let dropped = source.resource_observer();
    drop(source);
    assert!(dropped.payload_released());
    assert_eq!(observer.snapshot().read_roots, 1);
    drop(doc);
    assert!(!backing.unmap_succeeded());
    assert!(!observer.is_released());
    drop(raw_clone);
    assert!(observer.is_released());
    assert!(backing.unmap_succeeded());
    assert_eq!(backing.mapped_bytes(), 0);
    assert!(!backing.failed());
}

#[test]
#[cfg(feature = "paged-layout-breaks")]
fn cancelling_in_last_leaf_callback_rejects_partial_snapshot_result_and_cached_zero_range() {
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    let source = MarkdownLayoutTextSource::capture_cell_draft(&doc, &token).unwrap();
    let snapshot = source.snapshot_for_bidi();
    assert_eq!(snapshot.is_printable_ascii_range(0..1), Ok(true));
    let mut callbacks = 0;
    assert_eq!(
        snapshot.visit_byte_range(0..1, |_| {
            callbacks += 1;
            assert!(doc.cancel_cell_draft(&token));
            Ok(())
        }),
        Err(MarkdownDocumentBufferError::PreparedTextInputStale)
    );
    assert_eq!(callbacks, 1);
    assert_eq!(
        snapshot.byte_offset_for_utf16(0),
        Err(MarkdownDocumentBufferError::PreparedTextInputStale)
    );
}

#[test]
#[cfg(feature = "paged-layout-breaks")]
fn draft_source_and_read_capsule_keep_actual_send_sync_worker_boundary() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<MarkdownLayoutTextSource>();
    assert_send_sync::<MarkdownCellDraftReadCapsule>();
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(&mut doc, &token, "worker snapshot");
    let source = MarkdownLayoutTextSource::capture_cell_draft(&doc, &token).unwrap();
    let bytes = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        source
            .visit_byte_range(0..source.byte_len(), |part| {
                bytes.extend_from_slice(part);
                Ok(())
            })
            .unwrap();
        bytes
    })
    .join()
    .unwrap();
    assert_eq!(bytes, b"worker snapshot");
}

#[test]
#[cfg(feature = "paged-layout-breaks")]
fn pure_edit_draft_source_keeps_original_errno_ahead_of_revocation_and_cached_reads() {
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(&mut doc, &token, "pure edit");
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let backing = doc.original_backing_observer().unwrap();
    let source = MarkdownLayoutTextSource::capture_cell_draft(&doc, &token).unwrap();
    let snapshot = source.snapshot_for_bidi();
    let weak_guard = source.cell_draft_authority().unwrap();
    snapshot.copy_utf16_code_units(0, &mut [0]).unwrap();
    doc.text_snapshot()
        .visit_byte_range(0..1, |_| Ok(()))
        .unwrap();
    doc.original_snapshot
        .as_ref()
        .unwrap()
        .fail_next_reclaim_for_test();
    assert_eq!(
        doc.reclaim_original_read_windows_for_test(),
        Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
            libc::EIO
        ))
    );
    assert!(doc.cancel_cell_draft(&token));
    let error = MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO);
    assert_eq!(source.health(), Err(error));
    assert_eq!(snapshot.copy_utf16_code_units(0, &mut [0]), Err(error));
    assert_eq!(snapshot.byte_offset_for_utf16(0), Err(error));
    assert_eq!(snapshot.is_printable_ascii_range(0..0), Err(error));
    assert_eq!(weak_guard.health(), Err(error));
    drop(doc);
    assert!(!backing.unmap_succeeded());
    drop(source);
    drop(snapshot);
    assert!(observer.is_released());
    assert!(backing.unmap_succeeded());
    assert_eq!(backing.mapped_bytes(), 0);
    assert!(backing.failed());
}
