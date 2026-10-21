//! Admit the original sanitized candidate before retiring scenes, then consume that exact input serial once.

use super::*;
use crate::markdown_document_buffer::changes::MarkdownObservedEditError;

fn prepare(
    doc: &MarkdownDocumentBuffer,
    token: &MarkdownCellDraft,
) -> Result<MarkdownCellDraftPreparedCommit, MarkdownCellDraftError> {
    // Existing fixture convenience uses the same public capture/prepare boundary
    // as Native; it never clears marked input or manufactures an admitted ticket.
    doc.prepare_cell_draft_commit(doc.capture_cell_draft_edit(token)?, None)
}

fn assert_no_admission(observer: &MarkdownCellDraftResourceObserver) {
    // Observe the real prepared payload claim after consumption/drop, not merely
    // whether a later operation is allowed to occupy the original admission slot.
    let facts = observer.snapshot();
    assert!(!facts.prepared_commit_live);
    assert_eq!(facts.prepared_commit_bytes, 0);
}

struct Reject;
impl MarkdownTextChangeObserver for Reject {
    type Output = ();
    type Error = &'static str;

    fn changed(&mut self, _: MarkdownTextChange) -> Result<(), Self::Error> {
        Ok(())
    }

    fn finish(
        self,
        _: MarkdownPreparedText<'_>,
        _: MarkdownTextSelection,
    ) -> Result<(), Self::Error> {
        Err("reject admission publish")
    }
}

#[test]
fn long_admission_keeps_two_real_scenes_and_busy_consumption_unmaps_only_its_candidate() {
    // Two actual captured scenes keep original draft readers alive while a long
    // immutable replacement is prepared without modifying canonical storage.
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(&mut doc, &token, &"x".repeat(2 * 1024 * 1024));
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let before = doc.cell_draft_facts(&token).unwrap();
    let baseline = doc.storage_profile();
    let first = doc.capture_cell_draft(&token).unwrap();
    let second = doc.capture_cell_draft(&token).unwrap();
    let prepared = prepare(&doc, &token).unwrap();
    let candidate = prepared.replacement_backing_observer().unwrap();
    assert_eq!(candidate.mapped_bytes(), 2 * 1024 * 1024);
    assert_eq!(doc.cell_draft_facts(&token).unwrap(), before);
    assert_eq!(doc.storage_profile(), baseline);
    assert_eq!(observer.snapshot().read_roots, 2);
    assert_eq!(observer.snapshot().peak_prepared_commit_bytes, 0);
    assert!(observer.snapshot().prepared_commit_live);
    for scene in [&first, &second] {
        let mut tail = [0; 4];
        scene
            .copy_utf16_code_units(&scene.authority(), before.utf16_len - 4, &mut tail)
            .unwrap();
        assert_eq!(tail, [u16::from(b'x'); 4]);
    }
    // Preparation intentionally preserves both scenes. Only consumption of this
    // exact prepared ticket attempts publication, which must reject live readers.
    assert_eq!(canonical(&doc), SOURCE);
    assert!(matches!(
        doc.commit_prepared_cell_draft_observed(prepared, NoopObserver),
        Err(MarkdownCellDraftCommitError::Draft(
            MarkdownCellDraftError::ReadersBusy
        ))
    ));
    assert!(candidate.unmap_succeeded());
    assert_no_admission(&observer);
    assert_eq!(observer.snapshot().read_roots, 2);
    drop((first, second));
    // After actual reader retirement, prepare a new ticket and publish through
    // the same two phases as Native; the failed candidate cannot be reused.
    let prepared = prepare(&doc, &token).unwrap();
    doc.commit_prepared_cell_draft_observed(prepared, NoopObserver)
        .unwrap()
        .edit
        .unwrap();
    let expected = SOURCE.replacen("| a |", &format!("| {} |", "x".repeat(2 * 1024 * 1024)), 1);
    assert_eq!(canonical(&doc), expected);
    assert_eq!(
        doc.storage_profile().undo_entries,
        baseline.undo_entries + 1
    );
    doc.undo_text_edit().unwrap();
    assert_eq!(canonical(&doc), SOURCE);
    doc.redo_text_edit().unwrap();
    assert_eq!(canonical(&doc), expected);
    assert!(observer.is_released());
}

#[test]
fn admitted_ticket_with_live_readers_cannot_publish_and_is_really_returned() {
    // Reader rejection consumes the admitted ticket by value and returns its
    // actual payload while preserving both scene bytes and the editable branch.
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(&mut doc, &token, "candidate");
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let before = doc.cell_draft_facts(&token).unwrap();
    let baseline = doc.storage_profile();
    let first = doc.capture_cell_draft(&token).unwrap();
    let second = doc.capture_cell_draft(&token).unwrap();
    let prepared = prepare(&doc, &token).unwrap();
    assert!(observer.snapshot().prepared_commit_live);
    assert!(observer.snapshot().prepared_commit_bytes >= "candidate".len());
    assert_eq!(observer.snapshot().read_roots, 2);
    assert!(matches!(
        doc.commit_prepared_cell_draft_observed(prepared, NoopObserver),
        Err(MarkdownCellDraftCommitError::Draft(
            MarkdownCellDraftError::ReadersBusy
        ))
    ));
    assert_no_admission(&observer); // Failure retains no hidden replacement String for later reauthorization.
    assert_eq!(doc.cell_draft_facts(&token).unwrap(), before);
    assert_eq!(doc.storage_profile(), baseline);
    assert_eq!(read(&first), "candidate");
    assert_eq!(read(&second), "candidate");
    assert_eq!(canonical(&doc), SOURCE);
    drop((first, second));
    assert!(doc.cancel_cell_draft(&token));
}

#[test]
fn same_admission_after_scene_drop_publishes_once_with_one_canonical_undo() {
    // Capture the original serial while scenes still exist, then retire them and
    // consume that same admission. Sanitization and history must happen only once.
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(&mut doc, &token, "  new|中\r\n😀  ");
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let first = doc.capture_cell_draft(&token).unwrap();
    let second = doc.capture_cell_draft(&token).unwrap();
    let before = doc.storage_profile();
    let prepared = prepare(&doc, &token).unwrap();
    assert_eq!(doc.storage_profile(), before); // Admission publishes no canonical append, log or history.
    assert_eq!(doc.generation(), 0);
    drop((first, second));
    doc.commit_prepared_cell_draft_observed(prepared, NoopObserver)
        .unwrap()
        .edit
        .unwrap();
    let expected = SOURCE.replacen("| a |", "| new\\|中 😀 |", 1);
    assert_eq!(canonical(&doc), expected);
    assert_eq!(doc.generation(), 1);
    assert_eq!(doc.storage_profile().undo_entries, before.undo_entries + 1);
    assert!(!doc.has_cell_draft());
    assert!(observer.is_released());
    assert_no_admission(&observer);
    doc.undo_text_edit().unwrap();
    assert_eq!(canonical(&doc), SOURCE);
    doc.redo_text_edit().unwrap();
    assert_eq!(canonical(&doc), expected);
}

#[test]
fn only_one_live_admission_exists_and_drop_does_not_publish_or_change_draft() {
    // One actual admission occupies the slot. A second public prepare must fail
    // without changing the original payload; dropping it alone publishes nothing.
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(&mut doc, &token, "candidate");
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let before = doc.cell_draft_facts(&token).unwrap();
    let baseline = doc.storage_profile();
    let prepared = prepare(&doc, &token).unwrap();
    let capacity = observer.snapshot().prepared_commit_bytes;
    assert!(matches!(
        prepare(&doc, &token),
        Err(MarkdownCellDraftError::CommitBusy)
    ));
    assert_eq!(observer.snapshot().prepared_commit_bytes, capacity);
    assert_eq!(doc.cell_draft_facts(&token).unwrap(), before);
    assert_eq!(doc.storage_profile(), baseline);
    drop(prepared);
    assert_no_admission(&observer);
    let next = prepare(&doc, &token).unwrap();
    assert_eq!(observer.snapshot().prepared_commit_bytes, capacity);
    drop(next);
    assert_eq!(doc.cell_draft_facts(&token).unwrap(), before);
    assert_eq!(canonical(&doc), SOURCE);
    assert!(doc.cancel_cell_draft(&token));
}

#[test]
fn real_edit_selection_and_unmark_cannot_reauthorize_the_old_admission() {
    // All three real input mutations change the input serial, including a pure
    // selection or marked-state change that leaves content bytes unchanged.
    for change in 0..3 {
        let (_dir, mut doc) = document(SOURCE);
        let token = begin(&mut doc);
        replace(&mut doc, &token, "candidate");
        let observer = doc.cell_draft_resource_observer(&token).unwrap();
        let prepared = prepare(&doc, &token).unwrap();
        match change {
            0 => {
                edit(
                    &mut doc,
                    &token,
                    0..1,
                    "X",
                    MarkdownTextSelection::caret(1),
                    None,
                )
                .unwrap();
            }
            1 => {
                let edit = doc.capture_cell_draft_edit(&token).unwrap();
                doc.set_cell_draft_selection(edit, MarkdownTextSelection::caret(0))
                    .unwrap();
            }
            _ => {
                // Update only actual marked state and then publicly unmark it;
                // unchanged content_serial does not revive the old input serial.
                let content = doc.cell_draft_facts(&token).unwrap().content_serial;
                edit(
                    &mut doc,
                    &token,
                    0..0,
                    "",
                    MarkdownTextSelection::caret(0),
                    Some(0..1),
                )
                .unwrap();
                let edit = doc.capture_cell_draft_edit(&token).unwrap();
                doc.unmark_cell_draft(edit).unwrap();
                assert_eq!(
                    doc.cell_draft_facts(&token).unwrap().content_serial,
                    content
                );
            }
        }
        let current = doc.cell_draft_facts(&token).unwrap();
        let baseline = doc.storage_profile();
        // Consume the stale ticket through the public commit boundary and retain
        // the current draft/history; no new capture can bless the old admission.
        assert!(matches!(
            doc.commit_prepared_cell_draft_observed(prepared, NoopObserver),
            Err(MarkdownCellDraftCommitError::Draft(
                MarkdownCellDraftError::Stale
            ))
        ));
        assert_eq!(doc.cell_draft_facts(&token).unwrap(), current);
        assert_eq!(doc.storage_profile(), baseline);
        assert_eq!(canonical(&doc), SOURCE);
        assert_no_admission(&observer);
        assert!(doc.cancel_cell_draft(&token));
    }
}

#[test]
fn foreign_document_cannot_consume_or_reprepare_the_original_capsule() {
    // Identical source bytes in another actual document do not transfer the
    // original input capsule or prepared ticket into its draft ownership family.
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(&mut doc, &token, "candidate");
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let before = doc.cell_draft_facts(&token).unwrap();
    let (_other_dir, mut other) = document(SOURCE);
    let other_token = begin(&mut other);
    let original_edit = doc.capture_cell_draft_edit(&token).unwrap();
    assert!(matches!(
        other.prepare_cell_draft_commit(original_edit, None),
        Err(MarkdownCellDraftError::Stale)
    ));
    let prepared = prepare(&doc, &token).unwrap();
    assert!(matches!(
        other.commit_prepared_cell_draft_observed(prepared, NoopObserver),
        Err(MarkdownCellDraftCommitError::Draft(
            MarkdownCellDraftError::Stale
        ))
    ));
    assert_no_admission(&observer);
    assert_eq!(doc.cell_draft_facts(&token).unwrap(), before);
    assert_eq!(canonical(&doc), SOURCE);
    assert_eq!(canonical(&other), SOURCE);
    assert!(doc.cancel_cell_draft(&token));
    assert!(other.cancel_cell_draft(&other_token));
}

#[test]
fn marked_input_requires_public_unmark_before_preparation_and_commit() {
    // Native must finish its real marked-input protocol before admission. Public
    // preparation rejects active composition without clearing it or changing bytes.
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    edit(
        &mut doc,
        &token,
        0..1,
        "ni",
        MarkdownTextSelection::caret(2),
        Some(0..2),
    )
    .unwrap();
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let before = doc.cell_draft_facts(&token).unwrap();
    assert!(matches!(
        prepare(&doc, &token),
        Err(MarkdownCellDraftError::CompositionActive)
    ));
    assert_eq!(doc.cell_draft_facts(&token).unwrap(), before);
    assert_eq!(before.marked_range, Some(0..2));
    assert_no_admission(&observer);
    assert_eq!(observer.snapshot().peak_prepared_commit_bytes, 0);
    assert_eq!(canonical(&doc), SOURCE);
    // End composition through the existing public operation, then capture the
    // resulting original serial and use the same prepare/commit sequence as Native.
    let edit = doc.capture_cell_draft_edit(&token).unwrap();
    doc.unmark_cell_draft(edit).unwrap();
    assert_eq!(doc.cell_draft_facts(&token).unwrap().marked_range, None);
    let edit = doc.capture_cell_draft_edit(&token).unwrap();
    let prepared = doc.prepare_cell_draft_commit(edit, None).unwrap();
    doc.commit_prepared_cell_draft_observed(prepared, NoopObserver)
        .unwrap()
        .edit
        .unwrap();
    assert_eq!(canonical(&doc), SOURCE.replacen("| a |", "| ni |", 1));
    assert!(observer.is_released());
}

#[test]
fn unchanged_admission_does_not_call_observer_or_publish_selection_and_history() {
    // A real unchanged admission needs no canonical candidate transaction. The
    // rejecting observer would make any attempted publish observable as failure.
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    let before = doc.storage_profile();
    let selection = doc.selection();
    let prepared = prepare(&doc, &token).unwrap();
    assert!(
        doc.commit_prepared_cell_draft_observed(prepared, Reject)
            .unwrap()
            .edit
            .is_none()
    );
    assert_eq!(doc.storage_profile(), before);
    assert_eq!(doc.selection(), selection);
    assert_eq!(canonical(&doc), SOURCE);
    assert!(!doc.has_cell_draft());
}

#[test]
fn rejected_observer_rolls_back_real_canonical_candidate_and_keeps_local_history() {
    // Reject the actual canonical candidate after two local edits. Rollback must
    // retain draft bytes and local undo, while returning the prepared payload.
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(&mut doc, &token, "first");
    replace(&mut doc, &token, "candidate");
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let before = doc.cell_draft_facts(&token).unwrap();
    let baseline = doc.storage_profile();
    let prepared = prepare(&doc, &token).unwrap();
    assert!(matches!(
        doc.commit_prepared_cell_draft_observed(prepared, Reject),
        Err(MarkdownCellDraftCommitError::Edit(
            MarkdownObservedEditError::Observer("reject admission publish")
        ))
    ));
    assert!(!doc.has_prepared_text_input());
    assert_no_admission(&observer);
    assert_eq!(doc.cell_draft_facts(&token).unwrap(), before);
    assert_eq!(doc.storage_profile(), baseline);
    assert_eq!(canonical(&doc), SOURCE);
    // Existing public local-history restoration proves the original editable
    // branch survived the failed canonical publication.
    let edit = doc.capture_cell_draft_edit(&token).unwrap();
    doc.restore_cell_draft_history(edit, MarkdownCellDraftHistoryDirection::Undo)
        .unwrap()
        .unwrap();
    let reader = doc.capture_cell_draft(&token).unwrap();
    assert_eq!(read(&reader), "first");
    drop(reader);
    assert!(doc.cancel_cell_draft(&token));
}

#[test]
fn cancellation_retires_the_slot_until_the_actual_admission_string_drops() {
    // Cancelling the draft cannot destroy a separately owned admission payload.
    // The retired slot remains busy until that real last claim is returned.
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(&mut doc, &token, "candidate");
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let prepared = prepare(&doc, &token).unwrap();
    assert!(doc.cancel_cell_draft(&token));
    assert!(doc.has_cell_draft());
    assert!(!observer.snapshot().branch_live);
    assert!(observer.snapshot().prepared_commit_live);
    assert!(!observer.is_released());
    assert!(!doc.drain_cancelled_cell_draft());
    assert_eq!(
        doc.replace_utf16_range(0..0, "late", MarkdownTextSelection::caret(4)),
        Err(MarkdownDocumentBufferError::TextInputPrepared)
    );
    drop(prepared);
    assert!(observer.is_released());
    assert_no_admission(&observer);
    assert!(doc.drain_cancelled_cell_draft());
    assert!(!doc.has_cell_draft());
    assert_eq!(canonical(&doc), SOURCE);
}

#[test]
fn prepared_string_does_not_keep_doc_source_alive_but_still_has_a_real_release_claim() {
    // The ticket owns its finite replacement and atomic release claim, without
    // keeping the original document/mmap alive after the document is dropped.
    let (_dir, mut doc) = document(SOURCE);
    let backing = doc.original_backing_observer().unwrap();
    let token = begin(&mut doc);
    replace(&mut doc, &token, "candidate");
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let prepared = prepare(&doc, &token).unwrap();
    drop(doc);
    assert!(backing.unmap_succeeded());
    assert_eq!(backing.mapped_bytes(), 0);
    assert!(!backing.failed());
    assert!(!observer.snapshot().branch_live);
    assert!(observer.snapshot().prepared_commit_live);
    assert!(!observer.is_released());
    drop(prepared);
    assert!(observer.is_released());
    assert_no_admission(&observer);
}

#[test]
fn cancelled_ticket_cannot_publish_and_its_worker_drop_releases_only_its_own_claim() {
    // A cancelled ticket cannot publish. A later independent ticket may be dropped
    // on a real worker without running a Native callback or document transaction.
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(&mut doc, &token, "candidate");
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let prepared = prepare(&doc, &token).unwrap();
    assert!(doc.cancel_cell_draft(&token));
    assert!(matches!(
        doc.commit_prepared_cell_draft_observed(prepared, NoopObserver),
        Err(MarkdownCellDraftCommitError::Draft(
            MarkdownCellDraftError::Stale
        ))
    ));
    assert!(observer.is_released());
    assert!(doc.drain_cancelled_cell_draft());
    let token = begin(&mut doc);
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let prepared = prepare(&doc, &token).unwrap();
    // Admission retains no strong document/source edge; cross-thread drop returns
    // only its owned String and atomic claim, leaving the live branch unchanged.
    std::thread::spawn(move || drop(prepared)).join().unwrap();
    assert_no_admission(&observer);
    assert!(observer.snapshot().branch_live);
    assert_eq!(canonical(&doc), SOURCE);
    assert!(doc.cancel_cell_draft(&token));
}

#[test]
fn exact_utf16_limit_and_maximal_utf8_or_escaping_stay_within_the_single_sanitize_budget() {
    // Chinese UTF-8 and escaped pipes stress different expansion costs at the
    // exact UTF-16 limit, using one genuine prepared replacement per transaction.
    for scalar in ["中", "|"] {
        let (_dir, mut doc) = document(SOURCE);
        let token = begin(&mut doc);
        let raw = scalar.repeat(MARKDOWN_CELL_DRAFT_COMMIT_UTF16_UNITS);
        replace(&mut doc, &token, &raw);
        let observer = doc.cell_draft_resource_observer(&token).unwrap();
        let prepared = prepare(&doc, &token).unwrap();
        let capacity = observer.snapshot().prepared_commit_bytes;
        assert!(
            capacity <= crate::markdown_viewport_presentation::table_window::TABLE_WINDOW_MAX_BYTES
        );
        assert!(capacity >= raw.len());
        let expected = if scalar == "|" {
            "\\|".repeat(65536)
        } else {
            raw
        };
        doc.commit_prepared_cell_draft_observed(prepared, NoopObserver)
            .unwrap()
            .edit
            .unwrap();
        assert_eq!(
            canonical(&doc),
            SOURCE.replacen("| a |", &format!("| {expected} |"), 1)
        );
        assert!(observer.is_released());
    }
    // A supplementary-scalar input just beyond the small limit uses its actual
    // immutable replacement backing while preserving one canonical undo operation.
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(
        &mut doc,
        &token,
        &"😀".repeat(MARKDOWN_CELL_DRAFT_COMMIT_UTF16_UNITS / 2),
    );
    edit(
        &mut doc,
        &token,
        65536..65536,
        "x",
        MarkdownTextSelection::caret(65537),
        None,
    )
    .unwrap();
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let prepared = prepare(&doc, &token).unwrap();
    let candidate = prepared.replacement_backing_observer().unwrap();
    assert!(candidate.mapped_bytes() > 65536);
    assert_eq!(observer.snapshot().peak_prepared_commit_bytes, 0);
    assert_eq!(doc.cell_draft_facts(&token).unwrap().utf16_len, 65537);
    doc.commit_prepared_cell_draft_observed(prepared, NoopObserver)
        .unwrap()
        .edit
        .unwrap();
    assert_eq!(
        canonical(&doc),
        SOURCE.replacen("| a |", &format!("| {}x |", "😀".repeat(32768)), 1)
    );
    doc.undo_text_edit().unwrap();
    assert_eq!(canonical(&doc), SOURCE);
    assert!(observer.is_released());
}

#[test]
fn actual_source_error_precedes_cancelled_or_stale_admission_and_releases_the_string() {
    // Keep a real scene and stale/cancelled admission, then consume the existing
    // EIO injection through actual original-page reclaim. Source remains the first cause.
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(&mut doc, &token, "candidate");
    let reader = doc.capture_cell_draft(&token).unwrap();
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let backing = doc.original_backing_observer().unwrap();
    let prepared = prepare(&doc, &token).unwrap();
    let edit = doc.capture_cell_draft_edit(&token).unwrap();
    doc.set_cell_draft_selection(edit, MarkdownTextSelection::caret(0))
        .unwrap();
    assert!(doc.cancel_cell_draft(&token));
    doc.copy_utf16_code_units(0, &mut [0]).unwrap();
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
    assert!(matches!(
        doc.commit_prepared_cell_draft_observed(prepared, NoopObserver),
        Err(MarkdownCellDraftCommitError::Draft(
            MarkdownCellDraftError::Document(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
            )
        ))
    ));
    assert_no_admission(&observer);
    assert_eq!(observer.snapshot().first_source_errno, Some(libc::EIO));
    assert!(!observer.is_released());
    drop(reader);
    assert!(doc.drain_cancelled_cell_draft());
    drop(doc);
    assert!(observer.is_released());
    assert!(backing.unmap_succeeded());
    assert!(backing.failed());
}

#[test]
fn prepare_source_error_precedes_marked_and_busy_without_releasing_another_live_ticket() {
    // Source failure must precede marked/busy rejection without taking ownership
    // of a different live ticket or changing the original marked input.
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(&mut doc, &token, "candidate");
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let prepared = prepare(&doc, &token).unwrap();
    let capacity = observer.snapshot().prepared_commit_bytes;
    edit(
        &mut doc,
        &token,
        0..0,
        "",
        MarkdownTextSelection::caret(0),
        Some(0..1),
    )
    .unwrap();
    let edit = doc.capture_cell_draft_edit(&token).unwrap();
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
    assert!(matches!(
        doc.prepare_cell_draft_commit(edit, None),
        Err(MarkdownCellDraftError::Document(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
    assert!(observer.snapshot().prepared_commit_live);
    assert_eq!(observer.snapshot().prepared_commit_bytes, capacity);
    let Some(CellDraftSlot::Active(pending)) = &doc.cell_draft else {
        panic!("Source失败不得移走草稿")
    };
    assert_eq!(pending.marked_range, Some(0..1));
    drop(prepared);
    assert_no_admission(&observer);
    assert!(doc.cancel_cell_draft(&token));
    assert!(observer.is_released());
}

#[test]
fn source_failure_after_observer_finish_rolls_back_the_prepared_admission_publish() {
    // The observer consumes the existing reclaim injection at the actual
    // candidate boundary; this is not a naturally occurring kernel EIO.
    struct FailSource;
    impl MarkdownTextChangeObserver for FailSource {
        type Output = ();
        type Error = std::convert::Infallible;
        fn changed(&mut self, _: MarkdownTextChange) -> Result<(), Self::Error> {
            Ok(())
        }
        fn finish(
            self,
            candidate: MarkdownPreparedText<'_>,
            _: MarkdownTextSelection,
        ) -> Result<(), Self::Error> {
            candidate
                .document
                .original_snapshot
                .as_ref()
                .unwrap()
                .fail_next_reclaim_for_test();
            assert_eq!(
                candidate.document.reclaim_original_read_windows_for_test(),
                Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                    libc::EIO
                ))
            );
            Ok(())
        }
    }
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(&mut doc, &token, "candidate");
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let before = doc.cell_draft_facts(&token).unwrap();
    let baseline = doc.storage_profile();
    let root = doc.root.clone();
    let prepared = prepare(&doc, &token).unwrap();
    assert!(matches!(
        doc.commit_prepared_cell_draft_observed(prepared, FailSource),
        Err(MarkdownCellDraftCommitError::Draft(
            MarkdownCellDraftError::Document(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
            )
        ))
    ));
    assert!(!doc.has_prepared_text_input());
    assert!(piece_tree_links_share_root(&root, &doc.root));
    assert_eq!(doc.storage_profile(), baseline);
    let Some(CellDraftSlot::Active(pending)) = &doc.cell_draft else {
        panic!("原草稿必须仍在")
    };
    assert_eq!(pending.facts(), before);
    assert_no_admission(&observer);
    assert!(doc.cancel_cell_draft(&token));
    assert!(observer.is_released());
}

#[test]
/// Preserve sanitization across actual 4-KiB leaves, global Unicode trim and one save/undo/redo history.
fn long_sanitize_boundaries_save_and_history_preserve_exact_bytes() {
    let (directory, original) = document(SOURCE);
    drop(original);
    let workspace = crate::MarkdownWorkspace::new(directory.path().to_path_buf());
    let access = workspace.open_session().unwrap();
    // Save through this private workspace's canonical associated path; /var and
    // /private aliases cannot substitute another path for the actual save authority.
    let mut doc = access.open_document_buffer("cell-draft.md").unwrap();
    let token = begin(&mut doc);
    let prefix = "\u{a0}\u{2003}";
    let mut raw = format!("{prefix}{}", "a".repeat(4095 - prefix.len()));
    raw.push_str("\\|中 "); // The slash at byte 4095 escapes a pipe in the next actual leaf.
    raw.push_str(&"b".repeat(8190 - raw.len()));
    raw.push_str("\\\\|"); // An even backslash run still requires another slash to escape the following pipe.
    raw.push_str(&"c".repeat(12_287 - raw.len()));
    raw.push_str("\r\n"); // A leaf-final CR and next-leaf LF must sanitize to only one space.
    raw.push_str(&"正文😀".repeat(20_000));
    raw.push_str(&"\u{2003}\t\r\n".repeat(4096));
    replace(&mut doc, &token, &raw);
    let mut expected = format!(
        "{}\\|中 {}\\\\\\|{} ",
        "a".repeat(4095 - prefix.len()),
        "b".repeat(8190 - (4095 + "\\|中 ".len())),
        "c".repeat(12_287 - (8190 + "\\\\|".len()))
    );
    expected.push_str(&"正文😀".repeat(20_000));
    let prepared = prepare(&doc, &token).unwrap();
    let mapping = prepared.replacement_backing_observer().unwrap();
    doc.commit_prepared_cell_draft_observed(prepared, NoopObserver)
        .unwrap()
        .edit
        .unwrap();
    let full = SOURCE.replacen("| a |", &format!("| {expected} |"), 1);
    assert_eq!(canonical(&doc), full);
    assert_eq!(doc.storage_profile().allocated_edit_segment_bytes, 0);
    access.save_document_buffer(&mut doc).unwrap();
    assert_eq!(std::fs::read(doc.path()).unwrap(), full.as_bytes());
    assert!(!doc.is_dirty());
    assert_eq!(doc.storage_profile().undo_entries, 1);
    doc.undo_text_edit().unwrap();
    assert_eq!(canonical(&doc), SOURCE);
    assert!(doc.is_dirty());
    doc.redo_text_edit().unwrap();
    assert_eq!(canonical(&doc), full);
    assert!(!doc.is_dirty());
    assert!(mapping.peak_managed_resident_bytes() <= 64 * 1024);
    drop(doc);
    assert!(mapping.unmap_succeeded());
}

#[test]
/// A reclaim failure in the real unpublished candidate rejects it without poisoning the original raw source.
fn candidate_reclaim_failure_rolls_back_without_poisoning_the_original_draft() {
    struct FailCandidate;
    impl MarkdownTextChangeObserver for FailCandidate {
        type Output = ();
        type Error = std::convert::Infallible;
        fn changed(&mut self, _: MarkdownTextChange) -> Result<(), Self::Error> {
            Ok(())
        }
        fn finish(
            self,
            candidate: MarkdownPreparedText<'_>,
            _: MarkdownTextSelection,
        ) -> Result<(), Self::Error> {
            let MarkdownEditSegment::Frozen(backing) =
                candidate.document.edits.segments.last().unwrap().as_ref()
            else {
                panic!("长候选必须是实际封存段");
            };
            backing.fail_next_reclaim_for_test();
            assert_eq!(
                backing.reclaim_unpinned_for_test(),
                Err(crate::original_backing::OriginalBackingError::Unavailable(
                    libc::EIO
                ))
            );
            Ok(())
        }
    }
    let (_directory, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(&mut doc, &token, &"中😀".repeat(40_000));
    let facts = doc.cell_draft_facts(&token).unwrap();
    let profile = doc.storage_profile();
    let resources = doc.cell_draft_resource_observer(&token).unwrap();
    let prepared = prepare(&doc, &token).unwrap();
    let mapping = prepared.replacement_backing_observer().unwrap();
    assert!(matches!(
        doc.commit_prepared_cell_draft_observed(prepared, FailCandidate),
        Err(MarkdownCellDraftCommitError::Edit(
            MarkdownObservedEditError::Document(
                MarkdownDocumentBufferError::EditStorageUnavailable(libc::EIO)
            )
        ))
    ));
    assert!(mapping.unmap_succeeded());
    assert!(mapping.failed());
    assert_eq!(
        resources.snapshot().first_candidate_cleanup_errno,
        Some(libc::EIO)
    );
    assert_eq!(doc.cell_draft_facts(&token).unwrap(), facts);
    assert_eq!(doc.storage_profile(), profile);
    assert_eq!(canonical(&doc), SOURCE);
    assert_eq!(resources.snapshot().first_source_errno, None);
    let read = doc.capture_cell_draft(&token).unwrap();
    let mut tail = [0; 3];
    read.copy_utf16_code_units(&read.authority(), facts.utf16_len - 3, &mut tail)
        .unwrap();
    assert_eq!(tail, [0x4e2d, 0xd83d, 0xde00]);
    drop(read);
    replace(&mut doc, &token, "继续编辑");
    // Continue with real public editing after rollback, then prepare the new
    // input serial and consume exactly that ticket through Native's commit boundary.
    let edit = doc.capture_cell_draft_edit(&token).unwrap();
    let prepared = doc.prepare_cell_draft_commit(edit, None).unwrap();
    doc.commit_prepared_cell_draft_observed(prepared, NoopObserver)
        .unwrap()
        .edit
        .unwrap();
    assert_eq!(canonical(&doc), SOURCE.replacen("| a |", "| 继续编辑 |", 1));
    assert!(!resources.is_released()); // Later success cannot erase an earlier actual candidate-cleanup failure.
}

#[test]
/// Cancellation/document retirement leave the ticket's independent candidate alive until its final actual drop.
fn cancelled_long_ticket_retains_only_its_candidate_until_actual_drop() {
    let (_directory, mut doc) = document(SOURCE);
    let original = doc.original_backing_observer().unwrap();
    let token = begin(&mut doc);
    replace(&mut doc, &token, &"x".repeat(256 * 1024));
    let prepared = prepare(&doc, &token).unwrap();
    let mapping = prepared.replacement_backing_observer().unwrap();
    assert!(doc.cancel_cell_draft(&token));
    drop(doc);
    assert!(original.unmap_succeeded());
    assert_eq!(mapping.mapped_bytes(), 256 * 1024);
    drop(prepared);
    assert!(mapping.unmap_succeeded());
    assert_eq!(mapping.mapped_bytes(), 0);
}

#[test]
/// All-trimmed input creates no zero-length mapping; ordinary completion keeps the existing empty-cell semantics.
fn long_whitespace_sanitizes_to_the_existing_empty_cell_semantics() {
    let (_directory, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(&mut doc, &token, &"\u{2003}\r\n\t".repeat(40_000));
    let prepared = prepare(&doc, &token).unwrap();
    assert!(prepared.replacement_backing_observer().is_none());
    doc.commit_prepared_cell_draft_observed(prepared, NoopObserver)
        .unwrap()
        .edit
        .unwrap();
    assert_eq!(canonical(&doc), SOURCE.replacen("| a |", "|  |", 1));
    doc.undo_text_edit().unwrap();
    assert_eq!(canonical(&doc), SOURCE);
}

#[test]
/// Change directory/size limits only in a private child; real ENOENT/EFBIG must preserve the editable draft.
fn anonymous_output_io_failures_preserve_the_original_draft() {
    const CHILD: &str = "OMNI_CELL_OUTPUT_IO_CHILD";
    if let Some(kind) = std::env::var_os(CHILD) {
        let source =
            std::path::PathBuf::from(std::env::var_os("OMNI_CELL_OUTPUT_FIXTURE").unwrap());
        let scratch = std::path::PathBuf::from(std::env::var_os("TMPDIR").unwrap());
        let mut doc = MarkdownDocumentBuffer::open(&source, 32 * 1024 * 1024).unwrap();
        let token = begin(&mut doc);
        replace(&mut doc, &token, &"x".repeat(256 * 1024));
        let resources = doc.cell_draft_resource_observer(&token).unwrap();
        let first = doc.capture_cell_draft(&token).unwrap();
        let second = doc.capture_cell_draft(&token).unwrap();
        let before = doc.cell_draft_facts(&token).unwrap();
        let baseline = doc.storage_profile();
        // This --exact child has no competing tests; the real document, draft and
        // two readers stay alive across the operation. Each /dev/fd iterator ends
        // inside its query, and enumeration failure cannot count as release.
        let open_descriptors = || {
            let mut count = 0;
            for descriptor in std::fs::read_dir("/dev/fd").unwrap() {
                descriptor.unwrap();
                count += 1;
            }
            count
        };
        let descriptors_before = open_descriptors();
        let result = if kind == "create" {
            std::fs::remove_dir(&scratch).unwrap(); // Original snapshot/raw files are anonymous; removing empty scratch changes no bytes.
            prepare(&doc, &token)
        } else {
            // SAFETY: rlimit contains integer fields; this isolated child changes
            // only its own file-size limit, never another test or user process.
            let mut old: libc::rlimit = unsafe { std::mem::zeroed() };
            // SAFETY: old is a complete rlimit value; getrlimit writes it without retaining ownership.
            assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_FSIZE, &mut old) }, 0);
            let limited = libc::rlimit {
                rlim_cur: 4096,
                rlim_max: old.rlim_max,
            };
            // SAFETY: SIG_IGN is the standard handler constant. Only this child
            // temporarily requests real write failure as EFBIG, restoring the handler below.
            let signal = unsafe { libc::signal(libc::SIGXFSZ, libc::SIG_IGN) };
            // SAFETY: limited is a complete read-only rlimit; setrlimit changes only this process.
            assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_FSIZE, &limited) }, 0);
            let result = prepare(&doc, &token);
            // SAFETY: old came from this process; restoring its soft limit does not raise the original hard limit.
            assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_FSIZE, &old) }, 0);
            // SAFETY: Restore the same valid returned handler, without introducing another callback or process state.
            unsafe { libc::signal(libc::SIGXFSZ, signal) };
            result
        };
        // Real create/write failure returned and rlimit was restored. Candidate
        // output must close its actual FD, not just clear a prepared flag. Do not
        // inspect a stale numeric descriptor that could now identify another file.
        assert_eq!(open_descriptors(), descriptors_before);
        let expected = if kind == "create" {
            libc::ENOENT
        } else {
            libc::EFBIG
        };
        assert!(
            matches!(result, Err(MarkdownCellDraftError::Document(MarkdownDocumentBufferError::EditStorageUnavailable(errno))) if errno == expected)
        );
        assert_eq!(doc.cell_draft_facts(&token).unwrap(), before);
        assert_eq!(doc.storage_profile(), baseline);
        assert_eq!(resources.snapshot().first_source_errno, None);
        assert!(!resources.snapshot().prepared_commit_live);
        for reader in [&first, &second] {
            let mut tail = [0; 4];
            reader
                .copy_utf16_code_units(&reader.authority(), before.utf16_len - 4, &mut tail)
                .unwrap();
            assert_eq!(tail, [u16::from(b'x'); 4]);
        }
        drop((first, second));
        replace(&mut doc, &token, "仍可输入");
        assert_eq!(doc.cell_draft_facts(&token).unwrap().utf16_len, 4);
        assert_eq!(canonical(&doc), SOURCE);
        assert!(doc.cancel_cell_draft(&token));
        assert!(resources.is_released());
        return;
    }
    for kind in ["create", "write"] {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("cell.md");
        std::fs::write(&source, SOURCE).unwrap();
        let scratch = directory.path().join("scratch");
        std::fs::create_dir(&scratch).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "markdown_document_buffer::cell_draft::tests::commit_admission::anonymous_output_io_failures_preserve_the_original_draft", "--nocapture"])
            .env(CHILD, kind).env("OMNI_CELL_OUTPUT_FIXTURE", &source).env("TMPDIR", &scratch)
            .output().unwrap();
        assert!(
            output.status.success(),
            "真实匿名输出{kind}失败验证未通过：{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
