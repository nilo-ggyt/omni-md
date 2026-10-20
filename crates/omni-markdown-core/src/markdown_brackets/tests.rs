//! Differential context/precedence evidence plus canonical editing and finite-resource regressions.

use std::fs;
use std::time::Instant;

use serde::Deserialize;

use super::*;

/// Frozen synthetic baseline queries have explicit source coordinates and grammar admission scope.
#[derive(Deserialize)]
struct Reference {
    schema: u32,
    cases: Vec<Case>,
}

/// Every scalar-aligned caret in one synthetic note is exercised, including sides without brackets.
#[derive(Deserialize)]
struct Case {
    text: String,
    admitted: bool,
    queries: Vec<Query>,
}

/// The oracle records the real editor result without translating uncertainty into a fake expectation.
#[derive(Deserialize)]
struct Query {
    caret: usize,
    expected: Option<Expected>,
}

/// JSON ranges retain named endpoints rather than relying on Rust's private serialization layout.
#[derive(Deserialize)]
struct Expected {
    start: Range<usize>,
    end: Option<Range<usize>>,
    matched: bool,
}

/// Open a real immutable snapshot in an isolated test directory rather than a shared predictable file.
fn document(text: &str) -> (tempfile::TempDir, MarkdownDocumentBuffer) {
    let directory = tempfile::tempdir().expect("isolated bracket test directory");
    let path = directory.path().join("note.md");
    fs::write(&path, text).expect("synthetic Markdown source");
    let buffer = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).expect("bounded snapshot");
    (directory, buffer)
}

#[test]
fn admitted_contexts_and_every_caret_match_the_frozen_real_editor() {
    let reference: Reference =
        serde_json::from_str(include_str!("reference.json")).expect("frozen oracle schema");
    assert_eq!(reference.schema, 1);
    let mut differences = Vec::new();
    for case in reference.cases {
        // Unsupported fixtures still retain actual baseline outputs to make the omitted surface reviewable.
        let (_directory, buffer) = document(&case.text);
        for query in case.queries {
            // All runtime comparisons read the canonical buffer, including its unchanged CRLF positions.
            let actual = markdown_bracket_match(&buffer, query.caret).expect("valid scalar caret");
            let expected = query.expected.map(|value| MarkdownBracketMatch {
                start_utf16: value.start,
                end_utf16: value.end,
                matched: value.matched,
            });
            if case.admitted {
                // An admitted context must produce exact cues, not pass by silently returning uncertainty.
                let expected =
                    expected.map_or(MarkdownBracketQuery::NoMatch, MarkdownBracketQuery::Match);
                if actual != expected {
                    // Keep divergent source joins visible together instead of stopping at the first one.
                    differences.push(format!(
                        "source={:?} caret={} actual={actual:?} expected={expected:?}",
                        case.text, query.caret
                    ));
                }
            } else if let MarkdownBracketQuery::Match(found) = actual {
                // Unsupported syntax may omit cues, but any emitted cue must still equal the actual editor.
                assert_eq!(
                    Some(found),
                    expected,
                    "unsupported false cue: {:?} at {}",
                    case.text,
                    query.caret
                );
            }
        }
    }
    assert!(
        differences.is_empty(),
        "{} differences:\n{}",
        differences.len(),
        differences
            .iter()
            .take(40)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn literal_carets_do_not_split_ordinary_bracket_contexts_or_change_unicode_source() {
    let source = "(^中文😀)^ ²₃";
    let (directory, buffer) = document(source);
    // ^只是普通UTF8文字；跨越其位置的正文括号仍处于同一真实Paragraph上下文。
    let closing_byte = source.find(')').unwrap();
    let closing_utf16 = source[..closing_byte].encode_utf16().count();
    assert_eq!(
        markdown_bracket_match(&buffer, 1).unwrap(),
        MarkdownBracketQuery::Match(MarkdownBracketMatch {
            start_utf16: 0..1,
            end_utf16: Some(closing_utf16..closing_utf16 + 1),
            matched: true,
        })
    );
    // 线索查询只观察映射源，不能删除^或将Unicode上下标标量转成HTML标记。
    assert_eq!(
        fs::read(directory.path().join("note.md")).unwrap(),
        source.as_bytes()
    );
}

#[test]
fn live_piece_joins_undo_redo_and_unicode_validation_do_not_use_a_stale_context_cache() {
    let (_directory, mut buffer) = document("😀(a `)` b)");
    let initial = markdown_bracket_match(&buffer, 3).unwrap();
    assert_eq!(
        initial,
        MarkdownBracketQuery::Match(MarkdownBracketMatch {
            start_utf16: 2..3,
            end_utf16: Some(10..11),
            matched: true
        })
    );
    // Changing a delimiter turns code content back into paragraph content without touching a separate mirror.
    buffer
        .replace_utf16_range(5..6, " ", crate::MarkdownTextSelection::caret(6))
        .unwrap();
    assert_eq!(
        markdown_bracket_match(&buffer, 3).unwrap(),
        MarkdownBracketQuery::Match(MarkdownBracketMatch {
            start_utf16: 2..3,
            end_utf16: Some(6..7),
            matched: true
        })
    );
    assert!(buffer.undo().is_some());
    assert_eq!(markdown_bracket_match(&buffer, 3).unwrap(), initial);
    assert!(buffer.redo().is_some());
    assert_eq!(
        markdown_bracket_match(&buffer, 3).unwrap(),
        MarkdownBracketQuery::Match(MarkdownBracketMatch {
            start_utf16: 2..3,
            end_utf16: Some(6..7),
            matched: true
        })
    );
    // The public boundary rejects both a surrogate interior and an out-of-document coordinate before parsing.
    assert_eq!(
        markdown_bracket_match(&buffer, 1),
        Err(MarkdownDocumentBufferError::NotUnicodeBoundary)
    );
    assert_eq!(
        markdown_bracket_match(&buffer, buffer.utf16_len() + 1),
        Err(MarkdownDocumentBufferError::RangeOutsideDocument)
    );
}

#[test]
fn ordinary_short_complete_blocks_inside_large_notes_keep_cues_and_never_claim_unknown_eof() {
    let prefix = "ordinary paragraph without delimiters\n\n".repeat(40_000);
    let source = format!("{prefix}(short `)` body)\n\ntrailing paragraph\n");
    let (_directory, buffer) = document(&source);
    let start = prefix.len();
    assert_eq!(
        markdown_bracket_match(&buffer, start + 1).unwrap(),
        MarkdownBracketQuery::Match(MarkdownBracketMatch {
            start_utf16: start..start + 1,
            end_utf16: Some(start + 15..start + 16),
            matched: true
        })
    );
    for caret in [start, start + 1, start + 15, start + 16] {
        // Both sides of both ASCII handles work at an actual block start deep in a large document.
        assert!(matches!(
            markdown_bracket_match(&buffer, caret).unwrap(),
            MarkdownBracketQuery::Match(MarkdownBracketMatch { matched: true, .. })
        ));
    }
    // Searching outside a complete local block cannot prove that another Paragraph token has no partner.
    let (_directory, unmatched) = document(&format!("{prefix}(unmatched\n\nfar partner)\n"));
    assert_eq!(
        markdown_bracket_match(&unmatched, start + 1).unwrap(),
        MarkdownBracketQuery::ContextUnavailable
    );
    // Read-only classification leaves original/append/history storage unchanged; checkpoint metadata is bounded separately.
    let before = buffer.storage_profile();
    for _ in 0..100 {
        // Repeated foreground cues must not append source bytes or retain an accumulating token cache.
        assert!(matches!(
            markdown_bracket_match(&buffer, start + 1).unwrap(),
            MarkdownBracketQuery::Match(_)
        ));
    }
    let after = buffer.storage_profile();
    assert_eq!(format!("{before:?}"), format!("{after:?}"));
}

#[test]
fn inherited_html_is_conservatively_guarded_and_the_canonical_risk_bit_follows_edits_and_history() {
    let prefix = "ordinary paragraph\n\n".repeat(10_000);
    let source = format!("{prefix}(a `)` b)\n\n");
    let (_directory, mut buffer) = document(&source);
    assert!(!buffer.contains_ascii_less_than());
    assert!(matches!(
        markdown_bracket_match(&buffer, prefix.len() + 1).unwrap(),
        MarkdownBracketQuery::Match(_)
    ));
    // A preexisting far-open script spans many checkpoints; envelope-only classification would be wrong.
    let opener = "<script>\n\n";
    let (_html_directory, html) = document(&format!("{opener}{source}"));
    assert_eq!(
        markdown_bracket_match(&html, prefix.len() + opener.len() + 1).unwrap(),
        MarkdownBracketQuery::ContextUnavailable
    );
    // A nearby edit leaves an unaffected checkpoint within the acquisition bound for undo/removal tests.
    let insertion = prefix.len() - 4_000;
    buffer
        .insert_utf16(
            insertion,
            opener,
            crate::MarkdownTextSelection::caret(insertion + opener.len()),
        )
        .unwrap();
    assert!(buffer.contains_ascii_less_than());
    assert_eq!(
        markdown_bracket_match(&buffer, prefix.len() + opener.len() + 1).unwrap(),
        MarkdownBracketQuery::ContextUnavailable
    );
    assert!(buffer.undo().is_some());
    assert!(!buffer.contains_ascii_less_than());
    assert!(matches!(
        markdown_bracket_match(&buffer, prefix.len() + 1).unwrap(),
        MarkdownBracketQuery::Match(_)
    ));
    assert!(buffer.redo().is_some());
    assert!(buffer.contains_ascii_less_than());
    // Deleting the actual last introducer removes risk instead of retaining an append-buffer/history flag.
    buffer
        .replace_utf16_range(
            insertion..insertion + opener.len(),
            "",
            crate::MarkdownTextSelection::caret(insertion),
        )
        .unwrap();
    assert!(!buffer.contains_ascii_less_than());
    assert!(matches!(
        markdown_bracket_match(&buffer, prefix.len() + 1).unwrap(),
        MarkdownBracketQuery::Match(_)
    ));
    // Multiple leaves combine the flag with OR; deleting one introducer does not clear another one.
    buffer
        .insert_utf16(0, "<", crate::MarkdownTextSelection::caret(1))
        .unwrap();
    buffer
        .insert_utf16(
            buffer.utf16_len(),
            "<",
            crate::MarkdownTextSelection::caret(buffer.utf16_len() + 1),
        )
        .unwrap();
    buffer
        .replace_utf16_range(0..1, "", crate::MarkdownTextSelection::caret(0))
        .unwrap();
    assert!(buffer.contains_ascii_less_than());
    let last = buffer.utf16_len() - 1;
    buffer
        .replace_utf16_range(
            last..last + 1,
            "",
            crate::MarkdownTextSelection::caret(last),
        )
        .unwrap();
    assert!(!buffer.contains_ascii_less_than());
}

#[test]
fn complete_checkpoint_envelopes_preserve_full_fence_lengths_above_u8() {
    let prefix = "ordinary paragraph\n\n".repeat(10_000);
    let opening = "`".repeat(400);
    let shorter = "`".repeat(300);
    let fence = format!("{opening}\n(a\n{shorter}\n\nb)\n{opening}\n");
    let source = format!("{prefix}{fence}\ntrailing\n");
    let (_directory, buffer) = document(&source);
    let closing = prefix.len() + fence.find("b)").unwrap() + 1;
    // The shorter marker must remain code content, including after a blank line that follows it.
    let envelope = buffer
        .materialize_markdown_viewport(closing..closing + 1, 0)
        .unwrap();
    assert_eq!(
        envelope.semantic_status,
        MarkdownViewportSemanticStatus::Complete
    );
    assert_eq!(envelope.text, fence);
    assert_eq!(
        markdown_bracket_match(&buffer, closing).unwrap(),
        MarkdownBracketQuery::Match(MarkdownBracketMatch {
            start_utf16: closing..closing + 1,
            end_utf16: Some(prefix.len() + opening.len() + 1..prefix.len() + opening.len() + 2),
            matched: true
        })
    );
    // A checkpoint inside the long-lived fence must carry the same full delimiter length, not a byte cap.
    let long_fence = format!(
        "{opening}\n{}\n{shorter}\n\n(a)\n{opening}\n",
        "code\n\n".repeat(12_000)
    );
    let source = format!("{prefix}{long_fence}\ntrailing\n");
    let (_directory, buffer) = document(&source);
    let candidate = prefix.len() + long_fence.find("(a)").unwrap();
    let envelope = buffer
        .materialize_markdown_viewport(candidate..candidate + 1, 0)
        .unwrap();
    assert_eq!(envelope.text, long_fence);
    assert_eq!(
        markdown_bracket_match(&buffer, candidate + 1).unwrap(),
        MarkdownBracketQuery::BudgetExhausted
    );
}

#[test]
fn giant_line_and_exact_boundary_distinguish_budget_from_real_eof() {
    let (_directory, exact) = document(&format!(
        "({})",
        "x".repeat(MARKDOWN_BRACKET_SCAN_UTF16_UNITS - 2)
    ));
    assert!(matches!(
        markdown_bracket_match(&exact, 1).unwrap(),
        MarkdownBracketQuery::Match(MarkdownBracketMatch { matched: true, .. })
    ));
    let (_directory, unmatched) = document(&format!(
        "({}",
        "x".repeat(MARKDOWN_BRACKET_SCAN_UTF16_UNITS - 1)
    ));
    assert!(matches!(
        markdown_bracket_match(&unmatched, 1).unwrap(),
        MarkdownBracketQuery::Match(MarkdownBracketMatch {
            matched: false,
            end_utf16: None,
            ..
        })
    ));
    let (_directory, oversized) = document(&format!(
        "({})",
        "x".repeat(MARKDOWN_BRACKET_SCAN_UTF16_UNITS)
    ));
    assert_eq!(
        markdown_bracket_match(&oversized, 1).unwrap(),
        MarkdownBracketQuery::BudgetExhausted
    );
    // The baseline can overshoot its nominal limit on one iterator chunk; native must expose its stricter cap.
    let (_directory, huge) = document(&format!("({})", "x".repeat(1_000_000)));
    assert_eq!(
        markdown_bracket_match(&huge, 1).unwrap(),
        MarkdownBracketQuery::ContextUnavailable
    );
}

#[test]
#[ignore = "manual release profile; records algorithm cost, not perceived/native frame latency"]
fn profile_large_note_complete_block_queries() {
    let prefix = "ordinary paragraph without delimiters\n\n".repeat(200_000);
    let (_directory, buffer) = document(&format!("{prefix}(short `)` body)\n\ntrailing\n"));
    let first_start = Instant::now();
    assert!(matches!(
        markdown_bracket_match(&buffer, prefix.len() + 1).unwrap(),
        MarkdownBracketQuery::Match(_)
    ));
    let first_query_ns = first_start.elapsed().as_nanos();
    let envelope = buffer
        .materialize_markdown_viewport(prefix.len()..prefix.len() + 1, 0)
        .unwrap();
    let mut elapsed = Vec::with_capacity(1_000);
    for _ in 0..1_000 {
        // Measure only the pure query after document indexing, including checkpoint-backed acquisition.
        let start = Instant::now();
        assert!(matches!(
            markdown_bracket_match(&buffer, prefix.len() + 1).unwrap(),
            MarkdownBracketQuery::Match(_)
        ));
        elapsed.push(start.elapsed().as_nanos());
    }
    elapsed.sort_unstable();
    println!(
        "bracket pure-query profile bytes={} classification_envelope_utf16={} classification_envelope_bytes={} first_postindex_query_ns={} samples={} p50_ns={} p95_ns={} p99_ns={} max_ns={} profile={:?}",
        buffer.byte_len(),
        envelope.utf16_range.len(),
        envelope.text.len(),
        first_query_ns,
        elapsed.len(),
        elapsed[500],
        elapsed[950],
        elapsed[990],
        elapsed[999],
        buffer.storage_profile()
    );
}
