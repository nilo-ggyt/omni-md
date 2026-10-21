//! Regression tests preserved with their owning document/workspace boundary.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::symlink;

use tempfile::{TempDir, tempdir};

use crate::markdown_viewport_presentation::{
    MarkdownPresentationStyles, MarkdownViewportPresentation,
};

use super::{
    EDIT_SEGMENT_BYTES, MARKDOWN_BLOCK_CHECKPOINT_STRIDE_UTF16_UNITS, MAX_EDIT_HISTORY_ENTRIES,
    MAX_MATERIALIZED_UTF16_UNITS, MarkdownDocumentBuffer, MarkdownDocumentBufferError,
    MarkdownDocumentMetrics, MarkdownHistoryTextEdit, MarkdownLineEnding, MarkdownPieceTreeNode,
    MarkdownTextSelection, MarkdownViewportSemanticStatus, ORIGINAL_PIECE_BYTES, PieceTreeLink,
};

mod backing_windows;
mod original_spans;
mod streamed_selection;
mod viewport_single_line;

/// Own a real backing directory beside the mapped document for each isolated storage regression.
struct MarkdownBufferFixture {
    _directory: TempDir,
    buffer: MarkdownDocumentBuffer,
}

impl MarkdownBufferFixture {
    /// Create a document fixture under the standard test byte limit.
    fn new(content: &str) -> Self {
        // Delegate to the explicit-limit fixture so all tests share the same storage lifetime contract.
        Self::with_limit(content, 4 * 1_024 * 1_024)
    }

    /// Own a real temporary backing file for the entire mapped document fixture lifetime.
    fn with_limit(content: &str, maximum_bytes: u64) -> Self {
        // Keep the TempDir owner beside the buffer so its mmap backing path remains available until fixture teardown.
        let directory = tempdir().expect("Markdown buffer fixture");
        let path = directory.path().join("Document.md");
        fs::write(&path, content.as_bytes()).expect("write Markdown fixture");
        let buffer =
            MarkdownDocumentBuffer::open(&path, maximum_bytes).expect("open Markdown buffer");
        Self {
            _directory: directory,
            buffer,
        }
    }
}

#[test]
/// Protect the zero-copy original-file ownership contract on document open.
fn open_keeps_original_bytes_mapped_without_an_edit_copy() {
    // Use enough Unicode source to require multiple original leaves, then inspect retained storage immediately after open.
    let content = "# 标题\n\n正文 😀\n".repeat(4_000);
    let fixture = MarkdownBufferFixture::new(&content);
    let profile = fixture.buffer.storage_profile();

    // Original bytes must occupy only the mapping; a clean open allocates neither appended edit bytes nor edit segments.
    assert_eq!(profile.mapped_original_bytes, content.len());
    assert_eq!(profile.appended_edit_bytes, 0);
    assert_eq!(profile.allocated_edit_segment_bytes, 0);
    assert!(profile.piece_count > 1);
    assert!(!fixture.buffer.is_dirty());
    assert!(fixture.buffer.original_file_identity_is_unchanged());
    assert_eq!(streamed_text(&fixture.buffer), content);
}

#[test]
/// Verify scalar editing boundaries and NSString-compatible surrogate-unit reads use distinct contracts.
fn byte_and_utf16_locations_cover_cjk_emoji_and_combining_characters() {
    // Mix ASCII, CJK, a surrogate-pair emoji, and a combining mark with independently known byte/unit positions.
    let content = "a中😀e\u{301}z";
    let fixture = MarkdownBufferFixture::new(content);
    let buffer = &fixture.buffer;
    let boundaries = [(0, 0), (1, 1), (4, 2), (8, 4), (9, 5), (11, 6), (12, 7)];

    // Check every valid bidirectional boundary, then reject interior encoding positions while allowing raw code-unit reads.
    for (byte_offset, utf16_offset) in boundaries {
        // Each hand-calculated scalar boundary must round-trip in both coordinate directions.
        assert_eq!(buffer.utf16_offset_for_byte(byte_offset), Ok(utf16_offset));
        assert_eq!(buffer.byte_offset_for_utf16(utf16_offset), Ok(byte_offset));
    }
    assert_eq!(
        buffer.byte_offset_for_utf16(3),
        Err(MarkdownDocumentBufferError::NotUnicodeBoundary)
    );
    assert_eq!(buffer.utf16_code_unit_at(2), Ok(0xd83d));
    assert_eq!(buffer.utf16_code_unit_at(3), Ok(0xde00));
    assert_eq!(
        buffer.utf16_offset_for_byte(6),
        Err(MarkdownDocumentBufferError::NotUnicodeBoundary)
    );

    // A bounded range copy must match the corresponding independently encoded UTF-16 slice.
    let mut copied = [0_u16; 5];
    buffer
        .copy_utf16_code_units(1, &mut copied)
        .expect("copy UTF-16 range");
    assert_eq!(
        copied.as_slice(),
        &content.encode_utf16().collect::<Vec<_>>()[1..6]
    );

    // NSString may request only a low surrogate, so raw copy must preserve that exact unit without scalar alignment.
    let mut low_surrogate_only = [0_u16; 1];
    buffer
        .copy_utf16_code_units(3, &mut low_surrogate_only)
        .expect("copy inside surrogate pair");
    assert_eq!(low_surrogate_only, [0xde00]);
}

#[test]
/// Protect aggregate identities and bounded leaf construction at both empty and large single-line extremes.
fn empty_ascii_and_giant_single_line_documents_keep_correct_aggregates() {
    // Empty text owns no leaf and still represents one logical line.
    let empty = MarkdownBufferFixture::new("");
    assert_eq!(empty.buffer.byte_len(), 0);
    assert_eq!(empty.buffer.utf16_len(), 0);
    assert_eq!(empty.buffer.line_count(), 1);
    assert_eq!(empty.buffer.storage_profile().piece_count, 0);

    // A one-megabyte unbroken line must retain exact length and balanced chunked storage without inventing breaks.
    let giant_line = "x".repeat(1_048_576);
    let giant = MarkdownBufferFixture::new(&giant_line);
    assert_eq!(giant.buffer.byte_len(), giant_line.len());
    assert_eq!(giant.buffer.utf16_len(), giant_line.len());
    assert_eq!(giant.buffer.line_count(), 1);
    assert_eq!(
        giant.buffer.storage_profile().piece_count,
        giant_line
            .len()
            .div_ceil(ORIGINAL_PIECE_BYTES * super::ORIGINAL_SPAN_PIECES)
    );
    assert_tree_is_avl(&giant.buffer.root);
}

#[test]
/// Verify aggregate joins reconcile a CRLF split across two original leaves.
fn crlf_split_across_piece_boundaries_is_one_line_break() {
    // Place CR at the last byte of one original piece and LF at the first byte of the next.
    let mut content = "a".repeat(ORIGINAL_PIECE_BYTES - 1);
    content.push_str("\r\nnext\rstandalone\nlast");
    let fixture = MarkdownBufferFixture::new(&content);
    let buffer = &fixture.buffer;

    // Logical line counts and start coordinates must agree across paired and standalone line-ending forms.
    assert_eq!(buffer.line_count(), 4);
    assert_eq!(
        buffer.line_ending(),
        MarkdownLineEnding::CarriageReturnLineFeed
    );
    assert_eq!(buffer.byte_offset_for_line(1), Ok(ORIGINAL_PIECE_BYTES + 1));
    assert_eq!(
        buffer.byte_offset_for_line(2),
        Ok(content.find("standalone").unwrap())
    );
    assert_eq!(
        buffer.byte_offset_for_line(3),
        Ok(content.find("last").unwrap())
    );
    assert_eq!(buffer.line_for_byte(ORIGINAL_PIECE_BYTES - 1), Ok(0));
    assert_eq!(buffer.line_for_byte(ORIGINAL_PIECE_BYTES), Ok(1));
    assert_eq!(buffer.line_for_byte(ORIGINAL_PIECE_BYTES + 1), Ok(1));
}

#[test]
/// Differentially verify persistent editing, coordinates, line counts, and balance against an owned String oracle.
fn insert_delete_replace_and_random_edits_match_a_string_model() {
    // Use deterministic mixed Unicode and line-ending replacements so failures reproduce from a fixed seed.
    let mut fixture = MarkdownBufferFixture::new("start\n中文😀\r\nend");
    let mut expected = "start\n中文😀\r\nend".to_owned();
    let replacements = ["", "x", "中文", "😀", "\r\n", "e\u{301}", "**bold**"];
    let mut random = DeterministicRandom::new(0x4f4d_4e49_4d44);

    // After every randomized scalar-aligned replacement, compare exact streamed bytes, aggregates, and AVL invariants.
    for _ in 0..1_000 {
        // Use a fixed number of reproducible edits to cover different tree shapes without an unbounded fuzz run.
        let boundaries = utf8_boundaries(&expected);
        let first = random.index(boundaries.len());
        let second = random.index(boundaries.len());
        let (start_index, end_index) = if first <= second {
            // The first randomly selected boundary precedes the second and can be used as the edit start.
            (first, second)
        } else {
            // Reverse the selected endpoints so every generated interval is a valid ordered range.
            (second, first)
        };
        let byte_start = boundaries[start_index];
        let byte_end = boundaries[end_index];
        let utf16_start = expected[..byte_start].encode_utf16().count();
        let utf16_end = expected[..byte_end].encode_utf16().count();
        let replacement = replacements[random.index(replacements.len())];
        let caret = utf16_start + replacement.encode_utf16().count();

        fixture
            .buffer
            .replace_utf16_range(
                utf16_start..utf16_end,
                replacement,
                MarkdownTextSelection::caret(caret),
            )
            .expect("random piece-tree edit");
        expected.replace_range(byte_start..byte_end, replacement);

        assert_eq!(streamed_text(&fixture.buffer), expected);
        assert_eq!(fixture.buffer.byte_len(), expected.len());
        assert_eq!(fixture.buffer.utf16_len(), expected.encode_utf16().count());
        assert_eq!(fixture.buffer.line_count(), reference_line_count(&expected));
        assert_tree_is_avl(&fixture.buffer.root);
    }
}

#[test]
/// Verify history restoration returns exact TextKit edit geometry and the associated selection.
fn undo_redo_restore_shared_roots_and_selections() {
    // Commit two different replacement shapes so history must retain both shared roots and distinct prior selections.
    let mut fixture = MarkdownBufferFixture::new("abc😀");
    fixture
        .buffer
        .set_selection(MarkdownTextSelection::caret(3))
        .unwrap();
    fixture
        .buffer
        .insert_utf16(3, "-one", MarkdownTextSelection::caret(7))
        .unwrap();
    fixture
        .buffer
        .replace_utf16_range(0..3, "two", MarkdownTextSelection::caret(3))
        .unwrap();
    assert_eq!(streamed_text(&fixture.buffer), "two-one😀");

    // Undo and redo each entry while checking notification ranges, signed deltas, text, and saved-root cleanliness.
    assert_eq!(
        fixture.buffer.undo_text_edit(),
        Some(MarkdownHistoryTextEdit {
            replaced_utf16_range: 0..3,
            change_in_utf16_length: 0,
            selection_after: MarkdownTextSelection::caret(7),
        })
    );
    assert_eq!(streamed_text(&fixture.buffer), "abc-one😀");
    assert_eq!(
        fixture.buffer.undo_text_edit(),
        Some(MarkdownHistoryTextEdit {
            replaced_utf16_range: 3..7,
            change_in_utf16_length: -4,
            selection_after: MarkdownTextSelection::caret(3),
        })
    );
    assert_eq!(streamed_text(&fixture.buffer), "abc😀");
    assert!(!fixture.buffer.is_dirty());
    assert_eq!(
        fixture.buffer.redo_text_edit(),
        Some(MarkdownHistoryTextEdit {
            replaced_utf16_range: 3..3,
            change_in_utf16_length: 4,
            selection_after: MarkdownTextSelection::caret(7),
        })
    );
    assert_eq!(
        fixture.buffer.redo_text_edit(),
        Some(MarkdownHistoryTextEdit {
            replaced_utf16_range: 0..3,
            change_in_utf16_length: 0,
            selection_after: MarkdownTextSelection::caret(3),
        })
    );
    assert_eq!(streamed_text(&fixture.buffer), "two-one😀");
    assert!(fixture.buffer.is_dirty());
}

#[test]
/// Keep the convenience history API consistent with the exact text-edit history contract.
fn public_undo_redo_api_preserves_selection_contract() {
    // Create a replacement with a deliberately changed caret and verify history availability.
    let mut fixture = MarkdownBufferFixture::new("abc");
    assert_eq!(fixture.buffer.generation(), 0);
    assert!(!fixture.buffer.can_redo());
    fixture
        .buffer
        .insert_utf16(3, "😀", MarkdownTextSelection::caret(5))
        .unwrap();
    assert_eq!(fixture.buffer.generation(), 1);
    assert!(!fixture.buffer.can_redo());

    // Undo and redo must return the recorded selections and restore the corresponding text.
    assert_eq!(fixture.buffer.undo(), Some(MarkdownTextSelection::caret(0)));
    assert_eq!(fixture.buffer.generation(), 2);
    assert!(fixture.buffer.can_redo());
    assert_eq!(fixture.buffer.redo(), Some(MarkdownTextSelection::caret(5)));
    assert_eq!(fixture.buffer.generation(), 3);
    assert!(!fixture.buffer.can_redo());
    assert_eq!(streamed_text(&fixture.buffer), "abc😀");
}

#[test]
/// Reject a future caret inside a surrogate pair before appending any replacement bytes.
fn invalid_surrogate_selection_rejects_an_edit_without_allocating_edit_storage() {
    // Attempt an emoji insertion with an invalid interior caret, then compare text, selection, and edit storage with the untouched baseline.
    let mut fixture = MarkdownBufferFixture::new("");
    assert_eq!(
        fixture
            .buffer
            .insert_utf16(0, "😀", MarkdownTextSelection::caret(1)),
        Err(MarkdownDocumentBufferError::NotUnicodeBoundary)
    );
    assert_eq!(fixture.buffer.byte_len(), 0);
    assert_eq!(fixture.buffer.storage_profile().appended_edit_bytes, 0);
    assert_eq!(
        fixture
            .buffer
            .storage_profile()
            .allocated_edit_segment_bytes,
        0
    );
    assert!(!fixture.buffer.can_undo());
}

#[test]
/// Ensure a rejected native edit cannot overwrite the existing model selection or create history.
fn rejected_textkit_edit_preserves_the_preexisting_rust_selection() {
    // Choose a document already at its byte limit and retain a model selection distinct from the proposed native selection.
    let mut fixture = MarkdownBufferFixture::with_limit("abc", 3);
    let preexisting_selection = MarkdownTextSelection::caret(3);
    fixture
        .buffer
        .set_selection(preexisting_selection)
        .expect("seed Rust selection");

    // An oversized insertion must preserve the original caret, clean text, empty history, and unallocated edit storage.
    assert_eq!(
        fixture.buffer.replace_utf16_range_with_selection(
            0..0,
            "x",
            MarkdownTextSelection::caret(0),
            MarkdownTextSelection::caret(1),
        ),
        Err(MarkdownDocumentBufferError::DocumentTooLarge)
    );
    assert_eq!(fixture.buffer.selection(), preexisting_selection);
    assert_eq!(streamed_text(&fixture.buffer), "abc");
    assert!(!fixture.buffer.can_undo());
}

#[test]
/// Verify history entry eviction and lifetime appended-byte limits remain independent bounds.
fn history_and_session_edit_storage_are_bounded() {
    // Exceed the history entry count with small accepted edits and measure retained snapshots.
    let mut history_fixture = MarkdownBufferFixture::new("");
    for index in 0..(MAX_EDIT_HISTORY_ENTRIES + 20) {
        // Append beyond the history limit so eviction behavior is observed rather than assumed from a constant.
        let current = history_fixture.buffer.utf16_len();
        history_fixture
            .buffer
            .insert_utf16(current, "x", MarkdownTextSelection::caret(index + 1))
            .unwrap();
    }
    assert_eq!(
        history_fixture.buffer.storage_profile().undo_entries,
        MAX_EDIT_HISTORY_ENTRIES
    );

    // Use a tiny append budget to prove deleted live text does not reclaim lifetime edit capacity or permit unbounded churn.
    let mut edit_fixture = MarkdownBufferFixture::with_limit("", 8);
    edit_fixture
        .buffer
        .insert_utf16(0, "12345678", MarkdownTextSelection::caret(8))
        .unwrap();
    edit_fixture
        .buffer
        .delete_utf16_range(0..8, MarkdownTextSelection::caret(0))
        .unwrap();
    assert_eq!(
        edit_fixture
            .buffer
            .insert_utf16(0, "x", MarkdownTextSelection::caret(1)),
        Err(MarkdownDocumentBufferError::EditBufferLimitExceeded)
    );
    assert_eq!(streamed_text(&edit_fixture.buffer), "");
}

#[test]
/// Protect independent UTF-16 and UTF-8 viewport limits while preserving complete scalars.
fn viewport_materialization_is_bounded_and_does_not_split_surrogates() {
    // Place an emoji inside a large source so overscan begins and ends near a surrogate boundary.
    let mut content = "a".repeat(20_000);
    content.push('😀');
    content.push_str(&"b".repeat(20_000));
    let fixture = MarkdownBufferFixture::new(&content);
    let emoji_utf16 = 20_000;

    // The returned source and absolute range must include the complete emoji rather than a surrogate half.
    let materialized = fixture
        .buffer
        .materialize_viewport(emoji_utf16..emoji_utf16 + 2, 1)
        .expect("bounded viewport");
    assert_eq!(materialized.utf16_range, 19_999..20_003);
    assert_eq!(materialized.text, "a😀b");

    // A viewport larger than its explicit budget must reject instead of flattening the document.
    let oversized_fixture =
        MarkdownBufferFixture::new(&"x".repeat(MAX_MATERIALIZED_UTF16_UNITS + 1));
    assert_eq!(
        oversized_fixture
            .buffer
            .materialize_viewport(0..MAX_MATERIALIZED_UTF16_UNITS + 1, 0),
        Err(MarkdownDocumentBufferError::MaterializationLimitExceeded)
    );
}

#[test]
/// Require complete table envelopes before native semantic parsing.
fn markdown_viewport_materializes_the_complete_four_column_table() {
    // Embed a four-column Unicode table between unrelated blocks and target one interior character.
    let table = [
        "| 阶段 | 交付核心物料 | 周期 | 风险预案 |",
        "| :--- | :--- | :---: | ---: |",
        "| Phase 1 | 语义模型 | 2 周 | 低 |",
        "| Phase 2 | 原生渲染 | 3 周 | 中 |",
    ]
    .join("\n")
        + "\n";
    let content = format!("# 里程碑\n\n{table}\n后续说明\n");
    let target = utf16_offset_of(&content, "原生渲染");
    let table_start = utf16_offset_of(&content, "| 阶段");
    let table_end = table_start + table.encode_utf16().count();
    let fixture = MarkdownBufferFixture::new(&content);

    // Materialize from the interior caret so the scanner must recover the table start and end.
    let viewport = fixture
        .buffer
        .materialize_markdown_viewport(target..target + 1, 0)
        .expect("materialize complete table");

    // Complete status must contain exactly the whole table with its original document coordinates.
    assert_eq!(
        viewport.semantic_status,
        MarkdownViewportSemanticStatus::Complete
    );
    assert_eq!(viewport.utf16_range, table_start..table_end);
    assert_eq!(viewport.text, table);
}

#[test]
/// Protect header promotion when a sparse checkpoint lands between a table header and delimiter.
fn markdown_viewport_recovers_a_table_header_across_a_checkpoint() {
    // Position the table header at a checkpoint boundary to exercise restart lookbehind.
    let filler = "x".repeat(MARKDOWN_BLOCK_CHECKPOINT_STRIDE_UTF16_UNITS - 8);
    let table = [
        "Name | Value | Stage | Risk",
        ":--- | :---: | ---: | ---",
        "Omni | Fast | 2 | Low",
    ]
    .join("\n")
        + "\n";
    let content = format!("{filler}\n\n{table}\n尾声\n");
    let delimiter_start = utf16_offset_of(&content, ":---");
    let table_start = utf16_offset_of(&content, "Name | Value");
    let table_end = table_start + table.encode_utf16().count();
    let fixture = MarkdownBufferFixture::new(&content);

    // Confirm the selected restart state has enough header ownership to recover the true table start.
    let checkpoint = fixture
        .buffer
        .block_checkpoints
        .borrow()
        .checkpoint_at_or_before(delimiter_start);
    assert_eq!(checkpoint.utf16_offset, delimiter_start);

    // The resulting envelope must include the header and complete body despite the delimiter-centered request.
    let viewport = fixture
        .buffer
        .materialize_markdown_viewport(delimiter_start..delimiter_start + 1, 0)
        .expect("recover table header from checkpoint state");
    assert_eq!(
        viewport.semantic_status,
        MarkdownViewportSemanticStatus::Complete
    );
    assert_eq!(viewport.utf16_range, table_start..table_end);
    assert_eq!(viewport.text, table);
}

#[test]
/// Prevent an isolated table delimiter from being misinterpreted as a horizontal rule.
fn table_delimiter_viewport_is_presented_only_after_complete_block_recovery() {
    // Place a compact table after a large prefix and address only its delimiter row.
    let table = "阶段 | 交付物\n--- | ---\nPhase 1 | 语义模型\n";
    let content = format!("## 里程碑\n\n{table}\n后续说明\n");
    let delimiter_start = utf16_offset_of(&content, "--- | ---");
    let fixture = MarkdownBufferFixture::new(&content);

    // Recover the complete table envelope before constructing presentation metadata.
    let viewport = fixture
        .buffer
        .materialize_markdown_viewport(delimiter_start..delimiter_start + 1, 0)
        .expect("materialize complete table before parsing");
    assert_eq!(
        viewport.semantic_status,
        MarkdownViewportSemanticStatus::Complete
    );

    // Pass the recovered text with its absolute UTF-16 start into the presentation layer.
    let mut presentation = MarkdownViewportPresentation::default();
    presentation
        .rebuild(
            fixture.buffer.generation(),
            viewport.utf16_range,
            &viewport.text,
        )
        .expect("present recovered table block");

    // The delimiter must retain table semantics and hidden-marker roles within a complete table prototype.
    let delimiter = presentation
        .query(delimiter_start)
        .expect("table delimiter presentation");
    assert!(
        delimiter
            .styles
            .contains(MarkdownPresentationStyles::TABLE_DELIMITER)
    );
    assert!(
        !delimiter
            .styles
            .contains(MarkdownPresentationStyles::HORIZONTAL_RULE)
    );
    assert!(
        !delimiter
            .styles
            .contains(MarkdownPresentationStyles::HEADING_1)
    );
    assert!(
        !delimiter
            .styles
            .contains(MarkdownPresentationStyles::HEADING_2)
    );
}

#[test]
/// Keep blank lines from prematurely ending a fenced-code viewport envelope.
fn markdown_viewport_keeps_blank_lines_inside_a_fenced_code_block() {
    // Use two code lines separated by a blank line and target the second line.
    let fence = "```rust\nlet first = 1;\n\nlet second = 2;\n```\n";
    let content = format!("前言\n\n{fence}\n结尾\n");
    let target = utf16_offset_of(&content, "second");
    let fence_start = utf16_offset_of(&content, "```rust");
    let fence_end = fence_start + fence.encode_utf16().count();
    let fixture = MarkdownBufferFixture::new(&content);

    // The interior request must rewind to the opening fence and include its closing fence.
    let viewport = fixture
        .buffer
        .materialize_markdown_viewport(target..target + 1, 0)
        .expect("materialize complete fenced code block");

    // Complete status must preserve all fence bytes, blank lines, and absolute coordinates.
    assert_eq!(
        viewport.semantic_status,
        MarkdownViewportSemanticStatus::Complete
    );
    assert_eq!(viewport.utf16_range, fence_start..fence_end);
    assert_eq!(viewport.text, fence);
}

#[test]
/// Protect multiline list envelopes across CRLF, blank separators, and non-ASCII text.
fn markdown_viewport_preserves_crlf_unicode_list_boundaries() {
    // Build an indented continuation after a blank line with CJK and emoji content.
    let list = "- 中文😀\r\n  延续行\r\n\r\n  空行后的延续\r\n\r\n";
    let content = format!("说明\r\n\r\n{list}## 后续\r\n");
    let target = utf16_offset_of(&content, "😀");
    let list_start = utf16_offset_of(&content, "- 中文");
    let list_end = list_start + list.encode_utf16().count();
    let fixture = MarkdownBufferFixture::new(&content);

    // Request a small interior range so block expansion exercises list continuation state.
    let viewport = fixture
        .buffer
        .materialize_markdown_viewport(target..target + 2, 0)
        .expect("materialize CRLF Unicode list");

    // The complete envelope must preserve exact CRLF bytes and the full continued list.
    assert_eq!(
        viewport.semantic_status,
        MarkdownViewportSemanticStatus::Complete
    );
    assert_eq!(viewport.utf16_range, list_start..list_end);
    assert_eq!(viewport.text, list);
}

#[test]
/// Bound semantic work when a single fenced block exceeds the viewport parse budget.
fn markdown_viewport_falls_back_to_source_for_an_oversized_block() {
    // Create a fence larger than the semantic UTF-16 limit and target its middle.
    let body = "x".repeat(MAX_MATERIALIZED_UTF16_UNITS + 1_024);
    let content = format!("```text\n{body}\n```\n");
    let target = "```text\n".len() + MAX_MATERIALIZED_UTF16_UNITS / 2;
    let fixture = MarkdownBufferFixture::new(&content);

    // Materialize only a bounded source slice even though the enclosing fence cannot fit.
    let viewport = fixture
        .buffer
        .materialize_markdown_viewport(target..target + 1, 0)
        .expect("bounded source fallback");

    // The explicit fallback status prevents parsing the partial source as an independent Markdown block.
    assert_eq!(
        viewport.semantic_status,
        MarkdownViewportSemanticStatus::SourceFallback
    );
    assert_eq!(viewport.utf16_range, target..target + 1);
    assert_eq!(viewport.text, "x");
}

#[test]
/// Invalidate sparse scanner coordinates after forward editing and both history directions.
fn markdown_checkpoints_rebuild_after_edit_undo_and_redo() {
    // Place a table after enough source to create checkpoints and record its original position.
    let prefix = "p".repeat(70_000) + "\n\n";
    let table = "Key | Value\n--- | ---\nmode | fast\n";
    let content = format!("{prefix}{table}");
    let original_target = utf16_offset_of(&content, "fast");
    let insertion = "引言\n\n";
    let insertion_utf16 = insertion.encode_utf16().count();
    let mut fixture = MarkdownBufferFixture::new(&content);
    assert!(fixture.buffer.block_checkpoints.borrow().checkpoints.len() > 1);

    // Insert before the checkpoints and verify the full table moves by the replacement UTF-16 length.
    fixture
        .buffer
        .insert_utf16(0, insertion, MarkdownTextSelection::caret(insertion_utf16))
        .expect("insert before Markdown checkpoints");
    assert_eq!(
        fixture.buffer.block_checkpoints.borrow().checkpoints.len(),
        1
    );
    let edited = fixture
        .buffer
        .materialize_markdown_viewport(
            original_target + insertion_utf16..original_target + insertion_utf16 + 1,
            0,
        )
        .expect("materialize after edit");
    assert_eq!(edited.text, table);
    assert!(fixture.buffer.block_checkpoints.borrow().checkpoints.len() > 1);

    // Undo must restore the original table envelope and checkpoint coordinates.
    fixture.buffer.undo().expect("undo checkpoint edit");
    assert_eq!(
        fixture.buffer.block_checkpoints.borrow().checkpoints.len(),
        1
    );
    let undone = fixture
        .buffer
        .materialize_markdown_viewport(original_target..original_target + 1, 0)
        .expect("materialize after undo");
    assert_eq!(undone.text, table);

    // Redo must restore the shifted envelope without stale semantic ranges.
    fixture.buffer.redo().expect("redo checkpoint edit");
    assert_eq!(
        fixture.buffer.block_checkpoints.borrow().checkpoints.len(),
        1
    );
    let redone = fixture
        .buffer
        .materialize_markdown_viewport(
            original_target + insertion_utf16..original_target + insertion_utf16 + 1,
            0,
        )
        .expect("materialize after redo");
    assert_eq!(redone.text, table);
}

#[test]
/// Ensure persistence streams leaf-sized writes rather than a document-sized temporary body.
fn streaming_output_visits_piece_sized_chunks() {
    // Create several original leaves and insert Unicode edits into their middle.
    let original = "a".repeat(ORIGINAL_PIECE_BYTES * 3 + 17);
    let mut fixture = MarkdownBufferFixture::new(&original);
    fixture
        .buffer
        .insert_utf16(5, "中文😀", MarkdownTextSelection::caret(9))
        .unwrap();
    let mut writer = RecordingWriter::default();
    fixture.buffer.write_to(&mut writer).unwrap();

    // Compare streamed bytes with the independent expected String and assert the largest writer call remains piece-bounded.
    let mut expected = original;
    expected.insert_str(5, "中文😀");
    assert_eq!(String::from_utf8(writer.bytes).unwrap(), expected);
    assert!(writer.maximum_write_bytes <= ORIGINAL_PIECE_BYTES);
    assert!(
        fixture
            .buffer
            .storage_profile()
            .allocated_edit_segment_bytes
            <= EDIT_SEGMENT_BYTES
    );
}

#[test]
/// 打开前统一核实文件类型、UTF-8 编码与大小；拒绝后不能改写源文件或发布半成品文档。
fn secure_open_rejects_symlinks_non_files_invalid_utf8_and_oversize_documents() {
    // 全部输入属于同一个隔离临时目录，真实笔记库不参与这些失败与清理操作。
    let directory = tempdir().expect("secure open fixture");
    let note = directory.path().join("Note.md");
    let linked = directory.path().join("Linked.md");
    fs::write(&note, "safe").unwrap();
    symlink(&note, &linked).unwrap();

    // 符号链接与非普通文件不取得正文映射，避免把外部目标或特殊设备交给编辑会话。
    assert!(matches!(
        MarkdownDocumentBuffer::open(&linked, 1_024),
        Err(MarkdownDocumentBufferError::OpenUnavailable)
    ));
    assert!(matches!(
        MarkdownDocumentBuffer::open(directory.path(), 1_024),
        Err(MarkdownDocumentBufferError::NotRegularFile)
    ));

    // UTF-16 的两种字节序和损坏的 UTF-8 都明确拒绝；不猜测编码、不替换坏字节，
    // 更不能在打开时偷偷转码覆盖原文件。逐例复用同一隔离路径，不建立第二份编码状态。
    let invalid = directory.path().join("Invalid.md");
    for bytes in [
        &[0xff, 0xfe, 0x2d, 0x4e][..],
        &[0xfe, 0xff, 0x4e, 0x2d][..],
        &[0xe4, 0xb8][..],
    ] {
        fs::write(&invalid, bytes).unwrap();
        assert!(matches!(
            MarkdownDocumentBuffer::open(&invalid, 1_024),
            Err(MarkdownDocumentBufferError::InvalidUtf8Document)
        ));
        assert_eq!(fs::read(&invalid).unwrap(), bytes);
    }
    // 文件大小仍是独立的准入结果，不能把超限误报为编码问题。
    assert!(matches!(
        MarkdownDocumentBuffer::open(&note, 3),
        Err(MarkdownDocumentBufferError::DocumentTooLarge)
    ));
}

#[test]
/// Keep exact persisted bytes authoritative for revision identity independently of file identity.
fn revision_is_content_based_and_stable() {
    // Open identical bytes from distinct temporary files alongside one different byte sequence.
    let first = MarkdownBufferFixture::new("same");
    let second = MarkdownBufferFixture::new("same");
    let different = MarkdownBufferFixture::new("different");

    // Equal contents must hash equally and changed contents must hash differently in the stable hex representation.
    assert_eq!(
        first.buffer.original_revision(),
        second.buffer.original_revision()
    );
    assert_ne!(
        first.buffer.original_revision(),
        different.buffer.original_revision()
    );
    assert_eq!(first.buffer.original_revision().encode_hex().len(), 64);
}

/// Materialize a test oracle through the public streaming API only.
fn streamed_text(buffer: &MarkdownDocumentBuffer) -> String {
    // Tests may own a complete String oracle; production persistence remains leaf-streamed.
    let mut bytes = Vec::new();
    buffer
        .write_to(&mut bytes)
        .expect("stream Markdown document");
    String::from_utf8(bytes).expect("streamed Markdown remains UTF-8")
}

/// Translate a known fixture substring start into independently computed UTF-16 coordinates.
fn utf16_offset_of(source: &str, needle: &str) -> usize {
    // The substring is fixture-controlled, so a missing match is a test-definition failure.
    let byte_offset = source.find(needle).expect("Markdown fixture substring");
    source[..byte_offset].encode_utf16().count()
}

/// Enumerate every scalar boundary plus the final endpoint for randomized edit generation.
fn utf8_boundaries(text: &str) -> Vec<usize> {
    // Random edits choose only these boundaries so rejected-surrogate behavior is tested separately.
    let mut boundaries: Vec<_> = text
        .char_indices()
        .map(|(index, _)| {
            // Retain only a scalar-aligned source byte position for later bounded slicing.
            index
        })
        .collect();
    boundaries.push(text.len());
    boundaries
}

/// Count logical line breaks with an independent byte scanner for differential tests.
fn reference_line_count(text: &str) -> usize {
    // Use a simple linear oracle separate from the piece-tree aggregate implementation.
    let bytes = text.as_bytes();
    let mut line_count = 1;
    let mut index = 0;
    while index < bytes.len() {
        // Advance over each byte with CRLF consumed as one logical break.
        match bytes[index] {
            // A paired CRLF contributes one break and consumes both bytes.
            b'\r' if bytes.get(index + 1) == Some(&b'\n') => {
                line_count += 1;
                index += 2;
            }
            // A standalone CR or LF contributes one break and consumes one byte.
            b'\r' | b'\n' => {
                line_count += 1;
                index += 1;
            }
            // Ordinary bytes cannot affect the line-break oracle.
            _ => index += 1,
        }
    }
    line_count
}

/// Recursively validate stored height, aggregate joins, and bounded tree balance.
fn assert_tree_is_avl(root: &PieceTreeLink) {
    // Inspect each node using recomputed child results so cached values cannot validate themselves.
    /// Recompute tree shape and aggregate invariants from the leaves upward.
    fn inspect(node: &MarkdownPieceTreeNode) -> (u16, MarkdownDocumentMetrics, usize) {
        // Leaves and recursively validated child branches independently determine the expected metrics and height.
        match node {
            // A leaf supplies the base aggregate and height for recursive validation.
            MarkdownPieceTreeNode::Leaf(piece) => (1, piece.metrics, 1),
            MarkdownPieceTreeNode::OriginalSpan(span) => {
                // 独立累计目录中的真实小片，既验证压缩计量，也按实际物理树叶检查平衡。
                let metrics =
                    span.record_range()
                        .fold(MarkdownDocumentMetrics::default(), |sum, ordinal| {
                            let piece = span.piece(ordinal).unwrap();
                            assert!(piece.byte_len <= ORIGINAL_PIECE_BYTES);
                            MarkdownDocumentMetrics::joined(sum, piece.metrics)
                        });
                assert_eq!(span.metrics, metrics);
                (1, metrics, 1)
            }
            // A branch must match recomputed child aggregates, exact stored height, and the AVL height difference bound.
            MarkdownPieceTreeNode::Branch {
                left,
                right,
                metrics,
                height,
            } => {
                let (left_height, left_metrics, left_leaves) = inspect(left);
                let (right_height, right_metrics, right_leaves) = inspect(right);
                assert!(left_height.abs_diff(right_height) <= 1);
                assert_eq!(*height, left_height.max(right_height) + 1);
                assert_eq!(
                    *metrics,
                    MarkdownDocumentMetrics::joined(left_metrics, right_metrics)
                );
                (*height, *metrics, left_leaves + right_leaves)
            }
        }
    }

    // An absent root is the valid empty document; nonempty roots must satisfy all recursive invariants.
    let Some(root) = root else {
        // An empty document owns no tree and satisfies balance without recursion.
        return;
    };
    let (height, _, leaves) = inspect(root);
    let generous_logarithmic_bound = (usize::BITS - leaves.leading_zeros()) as u16 * 2 + 1;
    assert!(height <= generous_logarithmic_bound);
}

/// Fixed-seed generator used only to make randomized differential edits reproducible.
struct DeterministicRandom(u64);

impl DeterministicRandom {
    /// Retain one explicit seed without depending on process-global random state.
    fn new(seed: u64) -> Self {
        // The caller controls the entire subsequent edit sequence through this seed.
        Self(seed)
    }

    /// Advance a deterministic xorshift generator and select one bounded fixture index.
    fn index(&mut self, upper_bound: usize) -> usize {
        // The fixed mutation sequence keeps randomized regression failures reproducible without an external RNG dependency.
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 as usize) % upper_bound
    }
}

#[derive(Default)]
/// Test sink retaining exact streamed bytes and the largest single write size.
struct RecordingWriter {
    bytes: Vec<u8>,
    maximum_write_bytes: usize,
}

impl Write for RecordingWriter {
    /// Record the largest streaming chunk while retaining exact output for assertions.
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        // The test writer accepts each complete slice and measures per-call size rather than internal tree details.
        self.maximum_write_bytes = self.maximum_write_bytes.max(buffer.len());
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    /// Complete the in-memory test writer without filesystem side effects.
    fn flush(&mut self) -> io::Result<()> {
        // The sink owns no buffered filesystem resources requiring a flush operation.
        Ok(())
    }
}
