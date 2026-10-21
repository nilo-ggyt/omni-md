//! Actual table verification, same-document draft and canonical prepared-commit
//! regressions. Owned synthetic fixtures may materialize their oracle; product
//! reads retain their bounded Source contract. Existing reclaim-failure injection
//! is explicit below and is not evidence of an independently occurring OS failure.

use super::*;
use changes::{MarkdownPreparedText, MarkdownTextChange, MarkdownTextChangeObserver, NoopObserver};

mod commit_admission;
mod grouped_history;
mod input_read;
mod original_source;
mod row_append;
mod state_history;

const SOURCE: &str = "| H | C |\n| --- | --- |\n| a | b |\n";

fn document(source: &str) -> (tempfile::TempDir, MarkdownDocumentBuffer) {
    // The temporary directory owns the real file used by production mmap/pieces;
    // each case keeps it alive until its document resources have finished.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell-draft.md");
    std::fs::write(&path, source).unwrap();
    let doc = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap();
    (directory, doc)
}

fn window(doc: &MarkdownDocumentBuffer) -> VerifiedMarkdownTableWindow {
    // Drive the actual request over finite fixture input. No fabricated window or
    // direct struct construction grants table/cell authority to the tests.
    let mut request = doc
        .table_window_request(
            MarkdownTableWindowTarget::AtUtf16(0),
            0..doc.utf16_len(),
            1,
            0..0,
        )
        .unwrap();
    for _ in 0..doc.byte_len().saturating_mul(64) + 10000 {
        match request.advance(doc, false, 4096).unwrap() {
            MarkdownTableWindowStep::Progress { .. } => {}
            MarkdownTableWindowStep::Ready { .. } => {
                return request.take_ready_for(doc).unwrap().unwrap();
            }
            other => panic!("真实小表未认证: {other:?}"),
        }
    }
    panic!("有限表格请求未结束");
}

fn begin(doc: &mut MarkdownDocumentBuffer) -> MarkdownCellDraft {
    let verified = window(doc);
    doc.begin_cell_draft(&verified, 1, 0).unwrap()
}

fn canonical(doc: &MarkdownDocumentBuffer) -> String {
    // Whole output is a fixture oracle only. The operation under test still uses
    // the actual production document and its regular streaming writer.
    let mut bytes = Vec::new();
    doc.write_to(&mut bytes).unwrap();
    String::from_utf8(bytes).unwrap()
}

fn read(capsule: &MarkdownCellDraftReadCapsule) -> String {
    // Materialize only this test-owned expected result through the capsule's
    // bounded public visitor; never extract an unrestricted draft snapshot.
    let authority = capsule.authority();
    let facts = capsule.facts(&authority).unwrap();
    let mut bytes = Vec::new();
    let mut at = 0;
    while at < facts.byte_len {
        let end = (at + 4096).min(facts.byte_len);
        capsule
            .visit_byte_range(&authority, at..end, |part| {
                bytes.extend_from_slice(part);
                Ok(())
            })
            .unwrap();
        at = end;
    }
    String::from_utf8(bytes).unwrap()
}

fn edit(
    doc: &mut MarkdownDocumentBuffer,
    token: &MarkdownCellDraft,
    range: Range<usize>,
    text: &str,
    selection: MarkdownTextSelection,
    marked: Option<Range<usize>>,
) -> Result<MarkdownCellDraftFacts, MarkdownCellDraftError> {
    let capsule = doc.capture_cell_draft_edit(token)?;
    doc.replace_cell_draft(capsule, range, text, selection, marked)
}

fn replace(doc: &mut MarkdownDocumentBuffer, token: &MarkdownCellDraft, raw: &str) {
    let len = doc.cell_draft_facts(token).unwrap().utf16_len;
    edit(
        doc,
        token,
        0..len,
        raw,
        MarkdownTextSelection::caret(raw.encode_utf16().count()),
        None,
    )
    .unwrap();
}

#[test]
fn cell_inline_formats_match_original_plan_and_keep_one_local_undo_with_exact_selections() {
    let raw = "前中😀后\r\n管|\\尾";
    for action in [
        MarkdownFormatAction::Bold,
        MarkdownFormatAction::Italic,
        MarkdownFormatAction::Underline,
        MarkdownFormatAction::Strikethrough,
        MarkdownFormatAction::InlineCode,
        MarkdownFormatAction::Link,
    ] {
        for selection in [
            MarkdownTextSelection {
                anchor_utf16: 4,
                head_utf16: 1,
            },
            MarkdownTextSelection::caret(2),
        ] {
            // Use the original document-format plan as the finite oracle. Cell
            // formatting must not introduce another set of markers/wrapping rules.
            let (_oracle_dir, mut oracle) = document(raw);
            let expected_edit = oracle.apply_format(action, selection).unwrap();
            let expected = canonical(&oracle);
            let (_directory, mut doc) = document(SOURCE);
            let token = begin(&mut doc);
            replace(&mut doc, &token, raw);
            let edit = doc.capture_cell_draft_edit(&token).unwrap();
            doc.set_cell_draft_selection(edit, selection).unwrap();
            let before = doc.cell_draft_facts(&token).unwrap();
            let canonical_before = doc.storage_profile();
            let original_read = doc.capture_cell_draft(&token).unwrap();
            let original_authority = original_read.authority();
            let stale = doc.capture_cell_draft_edit(&token).unwrap();
            let edit = doc.capture_cell_draft_edit(&token).unwrap();
            let (formatted, actual_edit) = doc.format_cell_draft(edit, action).unwrap();
            assert_eq!(actual_edit, expected_edit);
            assert_eq!(formatted.selection, expected_edit.selection_after);
            assert_eq!(formatted.serial, before.serial + 1);
            assert_eq!(formatted.content_serial, before.content_serial + 1);
            assert_eq!(read(&doc.capture_cell_draft(&token).unwrap()), expected);
            assert_eq!(doc.storage_profile(), canonical_before);
            assert_eq!(canonical(&doc), SOURCE);
            assert_eq!(
                original_read.facts(&original_authority),
                Err(MarkdownCellDraftError::Stale)
            ); // The original owner still holds its payload, but pre-format text cannot become current content.
            drop(original_read);
            assert_eq!(
                doc.format_cell_draft(stale, action),
                Err(MarkdownCellDraftError::Stale)
            );
            let edit = doc.capture_cell_draft_edit(&token).unwrap();
            let undo = doc
                .restore_cell_draft_history(edit, MarkdownCellDraftHistoryDirection::Undo)
                .unwrap()
                .unwrap();
            assert_eq!(undo.facts.selection, selection);
            assert_eq!(read(&doc.capture_cell_draft(&token).unwrap()), raw);
            assert_eq!(
                undo.change_in_utf16_length,
                -actual_edit.change_in_utf16_length
            );
            // A caret move after Undo cannot rewrite the format group's Redo
            // target; restore the original plan's complete inner selection.
            let edit = doc.capture_cell_draft_edit(&token).unwrap();
            doc.set_cell_draft_selection(edit, MarkdownTextSelection::caret(0))
                .unwrap();
            let edit = doc.capture_cell_draft_edit(&token).unwrap();
            let redo = doc
                .restore_cell_draft_history(edit, MarkdownCellDraftHistoryDirection::Redo)
                .unwrap()
                .unwrap();
            assert_eq!(redo.facts.selection, actual_edit.selection_after);
            assert_eq!(
                redo.change_in_utf16_length,
                actual_edit.change_in_utf16_length
            );
            assert_eq!(read(&doc.capture_cell_draft(&token).unwrap()), expected);
            assert_eq!(doc.storage_profile(), canonical_before);
            assert!(doc.cancel_cell_draft(&token));
        }
    }
}

#[test]
fn cell_block_formats_and_active_composition_preserve_the_original_draft() {
    let (_directory, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    let before = doc.cell_draft_facts(&token).unwrap();
    let baseline = doc.storage_profile();
    for action in [
        MarkdownFormatAction::Heading1,
        MarkdownFormatAction::Heading2,
        MarkdownFormatAction::Heading3,
        MarkdownFormatAction::BulletList,
        MarkdownFormatAction::OrderedList,
        MarkdownFormatAction::TaskList,
        MarkdownFormatAction::Blockquote,
        MarkdownFormatAction::Divider,
        MarkdownFormatAction::CodeBlock,
        MarkdownFormatAction::Table,
    ] {
        let edit = doc.capture_cell_draft_edit(&token).unwrap();
        assert_eq!(
            doc.format_cell_draft(edit, action),
            Err(MarkdownCellDraftError::FormatRequiresDocument)
        );
        assert_eq!(doc.cell_draft_facts(&token).unwrap(), before);
    } // Native must finish the cell lifecycle before dispatching a document block action; no cell-local block syntax is invented.
    edit(
        &mut doc,
        &token,
        0..1,
        "ni",
        MarkdownTextSelection::caret(2),
        Some(0..2),
    )
    .unwrap();
    let marked = doc.cell_draft_facts(&token).unwrap();
    let edit = doc.capture_cell_draft_edit(&token).unwrap();
    assert_eq!(
        doc.format_cell_draft(edit, MarkdownFormatAction::Bold),
        Err(MarkdownCellDraftError::CompositionActive)
    );
    assert_eq!(doc.cell_draft_facts(&token).unwrap(), marked);
    assert_eq!(read(&doc.capture_cell_draft(&token).unwrap()), "ni");
    assert_eq!(doc.storage_profile(), baseline);
    assert!(doc.cancel_cell_draft(&token));
}

#[test]
fn cell_format_second_marker_budget_failure_rolls_back_real_candidate_and_delta() {
    let (_directory, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let chunk = "x".repeat(EDIT_SEGMENT_BYTES);
    for _ in 0..MARKDOWN_CELL_DRAFT_MAX_DELTA_BYTES / EDIT_SEGMENT_BYTES - 1 {
        replace(&mut doc, &token, &chunk);
    }
    replace(&mut doc, &token, &chunk[..EDIT_SEGMENT_BYTES - 2]);
    let selected = MarkdownTextSelection {
        anchor_utf16: 1,
        head_utf16: 0,
    };
    let edit = doc.capture_cell_draft_edit(&token).unwrap();
    doc.set_cell_draft_selection(edit, selected).unwrap();
    let before = doc.cell_draft_facts(&token).unwrap();
    let baseline = doc.storage_profile();
    let source = doc.capture_cell_draft(&token).unwrap();
    let root = doc.active_cell_draft(&token).unwrap().root.clone();
    let allocated = doc
        .active_cell_draft(&token)
        .unwrap()
        .edits
        .allocated_bytes();
    let delta = observer.snapshot().delta_bytes;
    assert_eq!(delta, MARKDOWN_CELL_DRAFT_MAX_DELTA_BYTES - 2);
    // Closing markers consume the actual final two append bytes; only the later
    // opening markers hit the cumulative allowance, exercising partial preparation rollback.
    let edit = doc.capture_cell_draft_edit(&token).unwrap();
    assert_eq!(
        doc.format_cell_draft(edit, MarkdownFormatAction::Bold),
        Err(MarkdownCellDraftError::Document(
            MarkdownDocumentBufferError::EditBufferLimitExceeded
        ))
    );
    let pending = doc.active_cell_draft(&token).unwrap();
    assert!(piece_tree_links_share_root(&root, &pending.root));
    assert_eq!(pending.edits.allocated_bytes(), allocated);
    assert_eq!(pending.edits.used_bytes, delta);
    assert_eq!(pending.facts(), before);
    assert_eq!(observer.snapshot().delta_bytes, delta);
    let mut tail = [0; 4];
    source
        .copy_utf16_code_units(&source.authority(), before.utf16_len - 4, &mut tail)
        .unwrap();
    assert_eq!(tail, [u16::from(b'x'); 4]); // Failed one-sided wrapping preserves original read authority, tail bytes and directed selection.
    drop(source);
    drop(root); // The temporary root-identity observation must not retain old tree metadata across later history/cancellation.
    let edit = doc.capture_cell_draft_edit(&token).unwrap();
    let undo = doc
        .restore_cell_draft_history(edit, MarkdownCellDraftHistoryDirection::Undo)
        .unwrap()
        .unwrap();
    assert_eq!(undo.facts.utf16_len, EDIT_SEGMENT_BYTES);
    assert_eq!(
        undo.facts.selection,
        MarkdownTextSelection::caret(EDIT_SEGMENT_BYTES)
    );
    assert_eq!(doc.storage_profile(), baseline);
    assert!(doc.cancel_cell_draft(&token));
    assert!(observer.is_released());
}

#[test]
fn cell_format_large_selection_shares_body_and_preserves_commit_admission() {
    let (_directory, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    let baseline = doc.storage_profile();
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    replace(&mut doc, &token, "");
    let chunk = "x".repeat(8192);
    for _ in 0..256 {
        let at = doc.cell_draft_facts(&token).unwrap().utf16_len;
        edit(
            &mut doc,
            &token,
            at..at,
            &chunk,
            MarkdownTextSelection::caret(at + chunk.len()),
            None,
        )
        .unwrap();
    }
    let length = 2 * 1024 * 1024;
    let selected = MarkdownTextSelection {
        anchor_utf16: length,
        head_utf16: 0,
    };
    let edit = doc.capture_cell_draft_edit(&token).unwrap();
    doc.set_cell_draft_selection(edit, selected).unwrap();
    let delta = observer.snapshot().delta_bytes;
    let edit = doc.capture_cell_draft_edit(&token).unwrap();
    let (formatted, change) = doc
        .format_cell_draft(edit, MarkdownFormatAction::Bold)
        .unwrap();
    assert_eq!(formatted.byte_len, length + 4);
    assert_eq!(change.replaced_utf16_range, 0..length);
    assert_eq!(change.change_in_utf16_length, 4);
    assert_eq!(observer.snapshot().delta_bytes, delta + 4);
    // Verify the complete shared result through original bounded reads, without
    // constructing another 2 MiB expected String in this case.
    let source = doc.capture_cell_draft(&token).unwrap();
    let authority = source.authority();
    let mut at = 0;
    while at < formatted.byte_len {
        let end = (at + 4096).min(formatted.byte_len);
        let mut offset = at;
        source
            .visit_byte_range(&authority, at..end, |bytes| {
                for &byte in bytes {
                    let expected = if offset < 2 || offset >= length + 2 {
                        b'*'
                    } else {
                        b'x'
                    };
                    assert_eq!(byte, expected);
                    offset += 1;
                }
                Ok(())
            })
            .unwrap();
        assert_eq!(offset, end);
        at = end;
    }
    drop(source);
    let edit = doc.capture_cell_draft_edit(&token).unwrap();
    let prepared = doc.prepare_cell_draft_commit(edit, None).unwrap();
    assert!(prepared.replacement_backing_observer().is_some());
    drop(prepared); // Continue checking raw local Undo after discarding actual sealed admission, without canonical publication.
    assert_eq!(doc.cell_draft_facts(&token).unwrap(), formatted);
    let edit = doc.capture_cell_draft_edit(&token).unwrap();
    let undo = doc
        .restore_cell_draft_history(edit, MarkdownCellDraftHistoryDirection::Undo)
        .unwrap()
        .unwrap();
    assert_eq!(undo.facts.byte_len, length);
    assert_eq!(undo.facts.selection, selected);
    assert_eq!(doc.storage_profile(), baseline);
    assert_eq!(canonical(&doc), SOURCE);
    assert!(doc.cancel_cell_draft(&token));
    assert!(observer.is_released());
}

#[test]
fn cell_format_revokes_old_prepared_commit_and_source_failure_still_precedes_format_context() {
    let (_directory, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let edit = doc.capture_cell_draft_edit(&token).unwrap();
    let prepared = doc.prepare_cell_draft_commit(edit, None).unwrap();
    let edit = doc.capture_cell_draft_edit(&token).unwrap();
    let (formatted, _) = doc
        .format_cell_draft(edit, MarkdownFormatAction::Bold)
        .unwrap();
    assert!(matches!(
        doc.commit_prepared_cell_draft_observed(prepared, NoopObserver),
        Err(MarkdownCellDraftCommitError::Draft(
            MarkdownCellDraftError::Stale
        ))
    ));
    assert_eq!(doc.cell_draft_facts(&token).unwrap(), formatted);
    let edit = doc.capture_cell_draft_edit(&token).unwrap();
    drop(doc.prepare_cell_draft_commit(edit, None).unwrap()); // The failed old ticket actually dropped, reopening this serial's unique admission slot.
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
    assert_eq!(
        doc.format_cell_draft(edit, MarkdownFormatAction::Heading1),
        Err(MarkdownCellDraftError::Document(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ); // The injected Source failure traversed actual reclaim first and must outrank a document-position format rejection.
    let pending = match &doc.cell_draft {
        Some(CellDraftSlot::Active(pending)) => pending,
        _ => panic!("源错误后原草稿仍须保留"),
    };
    assert_eq!(pending.facts(), formatted);
    assert_eq!(observer.snapshot().first_source_errno, Some(libc::EIO));
    assert!(doc.cancel_cell_draft(&token));
    assert!(observer.is_released());
}

#[test]
fn only_real_current_window_can_begin_and_canonical_writes_remain_blocked() {
    // Equal-byte foreign documents cannot exchange verified windows. While this
    // unique draft owns the baseline, canonical writes/selection changes are blocked.
    let (_dir, mut doc) = document(SOURCE);
    let (_other_dir, mut other) = document(SOURCE);
    let verified = window(&doc);
    assert!(matches!(
        other.begin_cell_draft(&verified, 1, 0),
        Err(MarkdownCellDraftError::Table(
            MarkdownTableWindowError::InvalidSource
        ))
    ));
    let token = doc.begin_cell_draft(&verified, 1, 0).unwrap();
    let before = doc.storage_profile();
    assert!(matches!(
        doc.begin_cell_draft(&verified, 1, 1),
        Err(MarkdownCellDraftError::Document(
            MarkdownDocumentBufferError::TextInputPrepared
        ))
    ));
    assert_eq!(
        doc.replace_utf16_range(0..0, "x", MarkdownTextSelection::caret(1)),
        Err(MarkdownDocumentBufferError::TextInputPrepared)
    );
    assert_eq!(
        doc.set_selection(MarkdownTextSelection::caret(1)),
        Err(MarkdownDocumentBufferError::TextInputPrepared)
    );
    assert_eq!(doc.storage_profile(), before);
    assert_eq!(canonical(&doc), SOURCE);
    assert!(doc.cancel_cell_draft(&token));
    doc.replace_utf16_range(0..0, "x", MarkdownTextSelection::caret(1))
        .unwrap();
    assert!(matches!(
        doc.begin_cell_draft(&verified, 1, 0),
        Err(MarkdownCellDraftError::Table(
            MarkdownTableWindowError::InvalidSource
        ))
    ));
}

#[test]
fn large_draft_is_accepted_and_can_shrink_without_polluting_canonical_append() {
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    let resources = doc.cell_draft_resource_observer(&token).unwrap();
    let baseline = doc.storage_profile();
    let revision = doc.current_revision().unwrap();
    replace(&mut doc, &token, "");
    let chunk = "x".repeat(8192);
    for _ in 0..256 {
        let at = doc.cell_draft_facts(&token).unwrap().utf16_len;
        edit(
            &mut doc,
            &token,
            at..at,
            &chunk,
            MarkdownTextSelection::caret(at + chunk.len()),
            None,
        )
        .unwrap();
    }
    assert_eq!(
        doc.cell_draft_facts(&token).unwrap().byte_len,
        2 * 1024 * 1024
    );
    assert_eq!(doc.storage_profile(), baseline);
    assert_eq!(doc.current_revision().unwrap(), revision);
    let prepared = doc
        .prepare_cell_draft_commit(doc.capture_cell_draft_edit(&token).unwrap(), None)
        .unwrap();
    let candidate = prepared.replacement_backing_observer().unwrap();
    drop(prepared); // Long admission succeeded; discarding it before shrinking raw input must not transfer its storage into canonical inventory.
    assert!(candidate.unmap_succeeded());
    let capsule = doc.capture_cell_draft(&token).unwrap();
    let authority = capsule.authority();
    let mut tail = [0; 4];
    capsule
        .copy_utf16_code_units(&authority, 2 * 1024 * 1024 - 4, &mut tail)
        .unwrap();
    assert_eq!(tail, [u16::from(b'x'); 4]);
    drop(capsule);
    replace(&mut doc, &token, "small");
    let input = doc.capture_cell_draft_edit(&token).unwrap();
    let prepared = doc.prepare_cell_draft_commit(input, None).unwrap();
    assert!(
        doc.commit_prepared_cell_draft_observed(prepared, NoopObserver)
            .unwrap()
            .edit
            .is_some()
    );
    assert_eq!(canonical(&doc), SOURCE.replacen("| a |", "| small |", 1));
    assert_eq!(
        doc.storage_profile().appended_edit_bytes - baseline.appended_edit_bytes,
        5
    );
    assert_eq!(
        doc.storage_profile().undo_entries - baseline.undo_entries,
        1
    );
    assert!(resources.is_released());
    assert!(resources.snapshot().peak_delta_bytes >= 2 * 1024 * 1024);
    doc.undo_text_edit().unwrap();
    assert_eq!(canonical(&doc), SOURCE);
    doc.redo_text_edit().unwrap();
    assert!(canonical(&doc).contains("| small |"));
}

#[test]
fn scalar_failures_roll_back_delta_serial_selection_and_marked() {
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(&mut doc, &token, "中😀e\u{301}");
    let facts = doc.cell_draft_facts(&token).unwrap();
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let delta = observer.snapshot().delta_bytes;
    assert_eq!(
        edit(
            &mut doc,
            &token,
            2..3,
            "x",
            MarkdownTextSelection::caret(1),
            None
        ),
        Err(MarkdownCellDraftError::Document(
            MarkdownDocumentBufferError::NotUnicodeBoundary
        ))
    );
    // Even after actual candidate append, a post-selection inside the new surrogate
    // pair must roll back this branch's storage and preserve its original serial.
    assert_eq!(
        edit(
            &mut doc,
            &token,
            0..facts.utf16_len,
            "😀",
            MarkdownTextSelection::caret(1),
            Some(0..2)
        ),
        Err(MarkdownCellDraftError::Document(
            MarkdownDocumentBufferError::NotUnicodeBoundary
        ))
    );
    assert_eq!(doc.cell_draft_facts(&token).unwrap(), facts);
    assert_eq!(observer.snapshot().delta_bytes, delta);
    let capsule = doc.capture_cell_draft(&token).unwrap();
    assert_eq!(read(&capsule), "中😀e\u{301}");
}

#[test]
fn read_authority_is_capture_and_serial_specific_with_fixed_two_payloads() {
    // Capsule clones share one actual payload. Two independent captures exhaust
    // the real slots, and editing revokes reads without prematurely freeing owners.
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(&mut doc, &token, "中😀");
    let old = doc.capture_cell_draft(&token).unwrap();
    let clone = old.clone();
    let second = doc.capture_cell_draft(&token).unwrap();
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    assert_eq!(observer.snapshot().read_roots, 2);
    assert!(matches!(
        doc.capture_cell_draft(&token),
        Err(MarkdownCellDraftError::ReadersBusy)
    ));
    assert_eq!(
        old.facts(&second.authority()),
        Err(MarkdownCellDraftError::Stale)
    );
    let mut low = [0];
    old.copy_utf16_code_units(&old.authority(), 2, &mut low)
        .unwrap();
    assert_eq!(low[0], 0xde00);
    assert_eq!(
        old.copy_utf16_code_units(&old.authority(), 0, &mut [0; 4097]),
        Err(MarkdownCellDraftError::ReadLimit)
    );
    replace(&mut doc, &token, "new");
    assert_eq!(
        old.facts(&old.authority()),
        Err(MarkdownCellDraftError::Stale)
    );
    drop(old);
    assert_eq!(observer.snapshot().read_roots, 2);
    drop(clone);
    drop(second);
    let latest = doc.capture_cell_draft(&token).unwrap();
    assert_eq!(read(&latest), "new");
    assert!(observer.snapshot().peak_read_roots <= 2);
}

#[test]
fn cancellation_keeps_retired_slot_until_last_actual_read_payload_drop() {
    // Cancelling drops the branch but not a live frozen payload. That real owner
    // keeps canonical mutation blocked until it drops and the retired slot drains.
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(&mut doc, &token, "uncommitted");
    let capsule = doc.capture_cell_draft(&token).unwrap();
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let baseline = doc.storage_profile();
    assert!(doc.cancel_cell_draft(&token));
    assert!(!observer.snapshot().branch_live);
    assert!(!observer.is_released());
    assert!(doc.has_cell_draft());
    assert_eq!(
        capsule.facts(&capsule.authority()),
        Err(MarkdownCellDraftError::Stale)
    );
    assert_eq!(
        doc.replace_utf16_range(0..0, "x", MarkdownTextSelection::caret(1)),
        Err(MarkdownDocumentBufferError::TextInputPrepared)
    );
    assert_eq!(canonical(&doc), SOURCE);
    assert_eq!(doc.storage_profile(), baseline);
    drop(capsule);
    assert!(observer.is_released());
    assert!(doc.drain_cancelled_cell_draft());
    assert!(!doc.has_cell_draft());
}

#[test]
fn token_drop_drains_and_unchanged_commit_does_not_add_history() {
    // A dropped weak token requests cancellation only. After explicit draining,
    // an unchanged prepared commit closes without a canonical edit or empty Undo.
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    let baseline = doc.storage_profile();
    drop(token);
    assert!(doc.has_cell_draft());
    assert!(doc.drain_cancelled_cell_draft());
    let token = begin(&mut doc);
    let input = doc.capture_cell_draft_edit(&token).unwrap();
    let prepared = doc.prepare_cell_draft_commit(input, None).unwrap();
    assert!(
        doc.commit_prepared_cell_draft_observed(prepared, NoopObserver)
            .unwrap()
            .edit
            .is_none()
    );
    assert_eq!(doc.storage_profile(), baseline);
    assert_eq!(canonical(&doc), SOURCE);
}

#[test]
fn actual_edited_segments_are_shared_and_draft_cow_never_writes_old_snapshot() {
    // Begin from actual canonical edit storage, then retain a real snapshot while
    // editing the draft. Original bytes must survive without canonical growth.
    let (_dir, mut doc) = document(SOURCE);
    let cell = SOURCE.find("| a |").unwrap() + 2;
    doc.replace_utf16_range(
        cell..cell + 1,
        "中😀",
        MarkdownTextSelection::caret(cell + 3),
    )
    .unwrap();
    let canonical_snapshot = doc.text_snapshot();
    let baseline = doc.storage_profile();
    let token = begin(&mut doc);
    let Some(CellDraftSlot::Active(pending)) = &doc.cell_draft else {
        unreachable!()
    };
    assert!(Arc::ptr_eq(
        &doc.edits.segments[0],
        &pending.edits.segments[0]
    ));
    replace(&mut doc, &token, "draft");
    let capsule = doc.capture_cell_draft(&token).unwrap();
    assert_eq!(read(&capsule), "draft");
    let mut original = [0; 3];
    canonical_snapshot
        .copy_utf16_code_units(cell, &mut original)
        .unwrap();
    assert_eq!(original, [0x4e2d, 0xd83d, 0xde00]);
    assert_eq!(doc.storage_profile(), baseline);
}

#[test]
fn held_reader_blocks_commit_and_observer_rejection_preserves_exact_draft() {
    // This existing pure observer rejects the actual prepared transaction's final
    // derived-output boundary. It is not an alternative text/Source implementation.
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
            Err("reject candidate")
        }
    }
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(&mut doc, &token, "candidate");
    let facts = doc.cell_draft_facts(&token).unwrap();
    let baseline = doc.storage_profile();
    let capsule = doc.capture_cell_draft(&token).unwrap();
    // Match Native's real ordering: preparation may coexist with the old Scene
    // reader, but canonical publication must wait for that actual payload to drop.
    let input = doc.capture_cell_draft_edit(&token).unwrap();
    let prepared = doc.prepare_cell_draft_commit(input, None).unwrap();
    assert!(matches!(
        doc.commit_prepared_cell_draft_observed(prepared, NoopObserver),
        Err(MarkdownCellDraftCommitError::Draft(
            MarkdownCellDraftError::ReadersBusy
        ))
    ));
    drop(capsule);
    let input = doc.capture_cell_draft_edit(&token).unwrap();
    let prepared = doc.prepare_cell_draft_commit(input, None).unwrap();
    assert!(matches!(
        doc.commit_prepared_cell_draft_observed(prepared, Reject),
        Err(MarkdownCellDraftCommitError::Edit(
            changes::MarkdownObservedEditError::Observer("reject candidate")
        ))
    ));
    assert_eq!(doc.cell_draft_facts(&token).unwrap(), facts);
    assert_eq!(doc.storage_profile(), baseline);
    assert!(!doc.has_prepared_text_input());
    assert_eq!(canonical(&doc), SOURCE);
    // Retry only after the failed candidate has actually left the unique slot;
    // this new preparation is explicit, using the same public path as Native.
    let input = doc.capture_cell_draft_edit(&token).unwrap();
    let prepared = doc.prepare_cell_draft_commit(input, None).unwrap();
    assert!(
        doc.commit_prepared_cell_draft_observed(prepared, NoopObserver)
            .unwrap()
            .edit
            .is_some()
    );
}

#[test]
fn sanitizer_and_implicit_cell_prefix_follow_original_table_rules() {
    // Finite Unicode/escaping fixtures exercise the original sanitizer. A missing
    // cell then compares its actual verified insertion plan with prepared commit.
    for (raw, escaped) in [
        (" a|b\r\nc\\|d ", "a\\|b c\\|d"),
        ("a\\\\|b", "a\\\\\\|b"),
        ("\u{2003}中\u{85}\u{2029}😀\u{2003}", "中\u{85}\u{2029}😀"),
    ] {
        let (_dir, mut doc) = document(SOURCE);
        let token = begin(&mut doc);
        replace(&mut doc, &token, raw);
        let input = doc.capture_cell_draft_edit(&token).unwrap();
        let prepared = doc.prepare_cell_draft_commit(input, None).unwrap();
        doc.commit_prepared_cell_draft_observed(prepared, NoopObserver)
            .unwrap();
        assert_eq!(
            canonical(&doc),
            SOURCE.replacen("| a |", &format!("| {escaped} |"), 1)
        );
    }
    let source = "| H | C |\n| --- | --- |\n| a\n";
    let (_dir, mut doc) = document(source);
    let verified = window(&doc);
    let expected = doc
        .plan_verified_table_cell_edit(&verified, 1, 1, "tail")
        .unwrap();
    let (range, replacement) = expected.replacement_for(&doc).unwrap();
    let mut oracle = source.to_owned();
    oracle.replace_range(range.clone(), replacement);
    let token = doc.begin_cell_draft(&verified, 1, 1).unwrap();
    assert_eq!(doc.cell_draft_facts(&token).unwrap().utf16_len, 0);
    replace(&mut doc, &token, "tail");
    let input = doc.capture_cell_draft_edit(&token).unwrap();
    let prepared = doc.prepare_cell_draft_commit(input, None).unwrap();
    doc.commit_prepared_cell_draft_observed(prepared, NoopObserver)
        .unwrap();
    assert_eq!(canonical(&doc), oracle);
}

#[test]
fn sanitizer_escape_parity_survives_actual_four_kib_piece_boundary() {
    // Place the backslash at the real logical piece boundary. Sanitization state
    // must survive that split instead of escaping a literal pipe a second time.
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    let raw = format!("{}\\|z\r\nq", "x".repeat(4095));
    replace(&mut doc, &token, &raw);
    let input = doc.capture_cell_draft_edit(&token).unwrap();
    let prepared = doc.prepare_cell_draft_commit(input, None).unwrap();
    doc.commit_prepared_cell_draft_observed(prepared, NoopObserver)
        .unwrap();
    assert_eq!(
        canonical(&doc),
        SOURCE.replacen("| a |", &format!("| {}\\|z q |", "x".repeat(4095)), 1)
    );
}

#[test]
fn draft_doc_drop_does_not_keep_document_and_final_capsule_really_unmaps_source() {
    // The frozen reader legitimately retains actual mapping ownership after the
    // document drops. Only releasing its final capsule can complete munmap.
    let (_dir, mut doc) = document(SOURCE);
    let backing = doc.original_backing_observer().unwrap();
    let token = begin(&mut doc);
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let capsule = doc.capture_cell_draft(&token).unwrap();
    drop(doc);
    assert!(!backing.unmap_succeeded());
    assert!(!observer.is_released());
    assert_eq!(
        capsule.facts(&capsule.authority()),
        Err(MarkdownCellDraftError::Stale)
    );
    drop(capsule);
    assert!(observer.is_released());
    assert!(backing.unmap_succeeded());
    assert_eq!(backing.mapped_bytes(), 0);
    assert!(!backing.failed());
    drop(token);
}

#[test]
fn true_source_errno_precedes_stale_cache_and_cancel_but_does_not_block_cleanup() {
    // Populate a real read first, then use the existing one-shot reclaim failure
    // seam and actually invoke reclaim. The resulting sticky Source failure must
    // outrank cache/stale/cancel outcomes without blocking final resource release.
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    let capsule = doc.capture_cell_draft(&token).unwrap();
    let authority = capsule.authority();
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    capsule
        .copy_utf16_code_units(&authority, 0, &mut [0])
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
    assert_eq!(
        capsule.copy_utf16_code_units(&authority, 0, &mut [0]),
        Err(MarkdownCellDraftError::Document(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    );
    assert_eq!(observer.snapshot().first_source_errno, Some(libc::EIO));
    let backing = doc.original_backing_observer().unwrap();
    drop(capsule);
    assert!(doc.drain_cancelled_cell_draft());
    drop(doc);
    assert!(observer.is_released());
    assert!(backing.unmap_succeeded());
    assert!(backing.failed());
}

#[test]
fn utf16_string_threshold_keeps_supplementary_boundary_and_commits_the_larger_frozen_draft() {
    // One extra unit crosses the bounded String threshold after complete emoji
    // pairs. The same prepared path must publish frozen storage and preserve Undo.
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    let boundary = "😀".repeat(MARKDOWN_CELL_DRAFT_COMMIT_UTF16_UNITS / 2);
    replace(&mut doc, &token, &boundary);
    edit(
        &mut doc,
        &token,
        65536..65536,
        "x",
        MarkdownTextSelection::caret(65537),
        None,
    )
    .unwrap();
    assert_eq!(doc.cell_draft_facts(&token).unwrap().utf16_len, 65537);
    let input = doc.capture_cell_draft_edit(&token).unwrap();
    let prepared = doc.prepare_cell_draft_commit(input, None).unwrap();
    doc.commit_prepared_cell_draft_observed(prepared, NoopObserver)
        .unwrap();
    assert_eq!(
        doc.storage_profile().appended_edit_bytes,
        boundary.len() + 1
    );
    assert!(matches!(
        doc.edits.segments[0].as_ref(),
        MarkdownEditSegment::Frozen(_)
    ));
    assert_eq!(
        canonical(&doc),
        SOURCE.replacen("| a |", &format!("| {boundary}x |"), 1)
    );
    doc.undo_text_edit().unwrap();
    assert_eq!(canonical(&doc), SOURCE);
}

#[test]
fn source_failure_after_observer_finish_cancels_real_prepared_candidate_and_keeps_draft() {
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
            let doc = candidate.document;
            // Actual pages have loaded unpinned slots. Trigger eviction after the
            // existing injection; the flag alone is not an observed I/O failure.
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
            Ok(())
        }
    }
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    replace(&mut doc, &token, "raw draft");
    let facts = doc.cell_draft_facts(&token).unwrap();
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let root = doc.root.clone();
    let before = doc.storage_profile();
    let input = doc.capture_cell_draft_edit(&token).unwrap();
    let prepared = doc.prepare_cell_draft_commit(input, None).unwrap();
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
    assert_eq!(doc.storage_profile(), before);
    let Some(CellDraftSlot::Active(pending)) = &doc.cell_draft else {
        panic!("原草稿必须保留")
    };
    assert_eq!(pending.facts(), facts);
    assert_eq!(observer.snapshot().first_source_errno, Some(libc::EIO));
    assert!(doc.cancel_cell_draft(&token));
    assert!(observer.is_released());
}

#[test]
fn edit_capsule_is_consumed_and_old_serial_or_foreign_nonce_cannot_reauthorize_coordinates() {
    // Actual input advances serial under the same nonce. Old local or equal-sized
    // foreign capsules cannot authorize edits against the current marked draft.
    let (_dir, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    let stale = doc.capture_cell_draft_edit(&token).unwrap();
    let current = doc.capture_cell_draft_edit(&token).unwrap();
    doc.replace_cell_draft(
        current,
        0..1,
        "new",
        MarkdownTextSelection::caret(3),
        Some(0..3),
    )
    .unwrap();
    let facts = doc.cell_draft_facts(&token).unwrap();
    assert_eq!(
        doc.replace_cell_draft(stale, 0..1, "bad", MarkdownTextSelection::caret(3), None),
        Err(MarkdownCellDraftError::Stale)
    );
    let (_other_dir, mut other) = document(SOURCE);
    let other_token = begin(&mut other);
    let foreign = other.capture_cell_draft_edit(&other_token).unwrap();
    assert_eq!(
        doc.replace_cell_draft(foreign, 0..1, "bad", MarkdownTextSelection::caret(3), None),
        Err(MarkdownCellDraftError::Stale)
    );
    assert_eq!(doc.cell_draft_facts(&token).unwrap(), facts);
    assert_eq!(canonical(&doc), SOURCE);
}
