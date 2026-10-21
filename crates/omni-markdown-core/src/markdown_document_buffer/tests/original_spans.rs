//! Actual storage, cross-directory coordinates, and persistent history for shared original-piece
//! directories. Complete oracle buffers belong only to finite synthetic test fixtures.

use super::*;
use std::ops::Range;

#[test]
fn actual_original_span_does_not_inflate_the_persistent_node() {
    // Compare actual production types rather than a mirrored layout. Ordinary edited leaves still
    // determine the node enum's largest variant.
    let node = std::mem::size_of::<MarkdownPieceTreeNode>();
    let piece = std::mem::size_of::<super::super::MarkdownPiece>();
    let span = std::mem::size_of::<super::super::OriginalSpan>();
    assert_eq!(node, piece);
    assert!(span <= piece);
    println!("event=actual_original_span_layout node={node} piece={piece} span={span}");
}

#[test]
fn original_spans_preserve_real_four_kib_borrows_and_all_scalar_coordinates() {
    // Cross several 64-KiB physical leaves and place split CRLF at both 4-KiB logical-piece and
    // 64-KiB directory boundaries.
    let mut source = "x".repeat(192 * 1024);
    for boundary in [4096, 65536, 131072] {
        source.replace_range(boundary - 1..boundary + 1, "\r\n");
    }
    for byte in [128, 65536 + 128, 131072 + 128] {
        // Equal-byte-length substitutions preserve those known boundaries while adding non-BMP
        // scalars, combining characters, and paragraph terminators.
        source.replace_range(byte..byte + 13, "中😀e\u{301}\u{2029}");
    }
    let fixture = MarkdownBufferFixture::new(&source);
    let snapshot = fixture.buffer.text_snapshot();
    let mut actual = Vec::new();
    // The caller-owned finite oracle may collect fixture output; every production callback must
    // still borrow one actual logical piece instead of the span's entire directory extent.
    snapshot
        .visit_byte_range(0..snapshot.byte_len(), |bytes| {
            assert!(bytes.len() <= ORIGINAL_PIECE_BYTES);
            actual.extend_from_slice(bytes);
            Ok(())
        })
        .unwrap();
    assert_eq!(actual, source.as_bytes());
    assert_tree_is_avl(&fixture.buffer.root);
    let mut utf16 = 0;
    for (ordinal, (byte, scalar)) in source.char_indices().enumerate() {
        // Check fixed-frequency scalar positions and both boundary neighborhoods. An interior
        // UTF-16 surrogate remains invalid for editing but valid for an NSString code-unit read.
        if ordinal % 127 == 0
            || [4096, 65536, 131072]
                .iter()
                .any(|at| byte.abs_diff(*at) < 16)
        {
            assert_eq!(snapshot.byte_offset_for_utf16(utf16), Ok(byte));
            assert_eq!(snapshot.utf16_offset_for_byte(byte), Ok(utf16));
        }
        if scalar.len_utf16() == 2 {
            assert_eq!(
                snapshot.byte_offset_for_utf16(utf16 + 1),
                Err(MarkdownDocumentBufferError::NotUnicodeBoundary)
            );
            assert_eq!(
                snapshot.utf16_code_unit_at(utf16 + 1),
                Ok(scalar.encode_utf16(&mut [0; 2])[1])
            );
        }
        utf16 += scalar.len_utf16();
    }
    assert_eq!(snapshot.byte_offset_for_utf16(utf16), Ok(source.len()));

    // An independent bytewise physical-line oracle verifies CRLF joining. Text-checking paragraph
    // boundaries below still use original CR/LF/PS terminators rather than physical-line counts.
    let mut line_starts = vec![0];
    let mut cursor = 0;
    while cursor < source.len() {
        match source.as_bytes()[cursor] {
            b'\r' if source.as_bytes().get(cursor + 1) == Some(&b'\n') => {
                cursor += 2;
                line_starts.push(cursor);
            }
            b'\r' | b'\n' => {
                cursor += 1;
                line_starts.push(cursor);
            }
            _ => cursor += 1,
        }
    }
    assert_eq!(fixture.buffer.line_count(), line_starts.len());
    for (line, byte) in line_starts.into_iter().enumerate() {
        assert_eq!(fixture.buffer.byte_offset_for_line(line), Ok(byte));
    }
    let units: Vec<_> = source.encode_utf16().collect();
    for start in (0..=units.len()).step_by(113) {
        for end in [start, (start + 1).min(units.len())] {
            let expected = checking_oracle(&units, start..end);
            assert_eq!(
                snapshot.text_checking_paragraph_range(start..end),
                Ok(expected)
            );
        }
    }
}

fn checking_oracle(units: &[u16], request: Range<usize>) -> Range<usize> {
    // This independent finite array exists only in tests. Keep the platform's strict-before start,
    // empty-caret anchor, and complete CRLF endpoint rules.
    let terminator = |unit: u16| matches!(unit, 0x000d | 0x000a | 0x2029);
    let start = units[..request.start]
        .iter()
        .rposition(|unit| terminator(*unit))
        .map_or(0, |at| at + 1);
    let anchor = if request.is_empty() {
        request.end
    } else {
        request.end - 1
    };
    let Some(at) = units[anchor..]
        .iter()
        .position(|unit| terminator(*unit))
        .map(|at| anchor + at)
    else {
        return start..units.len();
    };
    let end = at + 1 + usize::from(units[at] == 0x000d && units.get(at + 1) == Some(&0x000a));
    start..end
}

#[test]
fn shared_original_directories_keep_all_history_entries_and_frozen_resource_release() {
    let original = "中文😀a\r\nb e\u{301}\u{2029}".repeat(8_000);
    let mut fixture = MarkdownBufferFixture::new(&original);
    let observer = fixture.buffer.original_backing_observer().unwrap();
    let frozen = fixture.buffer.text_snapshot();
    let mut expected = original.clone();
    let mut history = Vec::new();
    for ordinal in 0..MAX_EDIT_HISTORY_ENTRIES {
        // Edit actual scalar boundaries near different original directories. History preserves
        // persistent roots instead of expanding every original logical piece.
        let at = (ordinal * 1597) % expected.len();
        let at = (at..=expected.len())
            .find(|at| expected.is_char_boundary(*at))
            .unwrap();
        let unit = expected[..at].encode_utf16().count();
        history.push(expected.clone());
        expected.insert_str(at, "增😀");
        fixture
            .buffer
            .insert_utf16(unit, "增😀", MarkdownTextSelection::caret(unit + 3))
            .unwrap();
        assert_tree_is_avl(&fixture.buffer.root);
    }
    assert_eq!(
        fixture.buffer.storage_profile().undo_entries,
        MAX_EDIT_HISTORY_ENTRIES
    );
    for before in history.iter().rev() {
        // Verify each of 128 actual undo states and AVL invariants, not merely a coincidentally
        // matching final digest.
        fixture.buffer.undo_text_edit().unwrap();
        assert_eq!(streamed_text(&fixture.buffer), *before);
        assert_tree_is_avl(&fixture.buffer.root);
    }
    assert!(!fixture.buffer.can_undo());
    assert_eq!(streamed_text(&fixture.buffer), original);
    // The frozen reader legitimately retains original bytes after the document/history owner ends.
    // Directory sharing alone neither extends read authority nor proves the mapping was released.
    drop(fixture);
    assert!(!observer.unmap_succeeded());
    let mut restored = Vec::new();
    frozen
        .visit_byte_range(0..frozen.byte_len(), |bytes| {
            assert!(bytes.len() <= ORIGINAL_PIECE_BYTES);
            restored.extend_from_slice(bytes);
            Ok(())
        })
        .unwrap();
    assert_eq!(restored, original.as_bytes());
    // End the actual last reader before checking successful munmap and the retained failure receipt;
    // dropping an internal payload by itself would not prove this OS operation succeeded.
    drop(frozen);
    assert!(observer.unmap_succeeded());
    assert_eq!(observer.mapped_bytes(), 0);
    assert!(!observer.failed());
}
