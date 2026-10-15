//! Actual preserved-command differential and canonical markup transaction regressions.

use super::*;
use serde::Deserialize;

/// Oracle rows contain only original editor inputs/outputs and primary native selection coordinates.
#[derive(Deserialize)]
struct Reference {
    action: String,
    input: String,
    handled: bool,
    anchor: usize,
    head: usize,
    text: String,
    anchor_after: usize,
    head_after: usize,
}

/// Freeze actual command behavior rather than deriving expected results from the new Rust scanner.
#[test]
fn markup_commands_match_actual_codemirror_corpus() {
    // Every input is synthetic; the external generator invokes the real preserved package exports.
    let corpus: serde_json::Value =
        serde_json::from_str(include_str!("reference.json")).expect("oracle");
    assert_eq!(
        corpus["provenance"]["@codemirror/lang-markdown"]["version"],
        "6.5.2"
    );
    assert_eq!(
        corpus["provenance"]["@codemirror/lang-markdown"]["sha256"],
        "fbef0a4eb11d458a47783da78b9fa248807c02040d53f891f087ecd82d9d08a7"
    );
    let cases: Vec<Reference> = serde_json::from_value(corpus["cases"].clone()).expect("cases");
    assert_eq!(cases.len(), 4328);
    let directory = tempfile::tempdir().expect("fixture");
    let path = directory.path().join("note.md");
    let mut mismatches = 0;
    let mut reported = std::collections::BTreeSet::new();
    for case in cases {
        // A fresh immutable snapshot/tree ensures no command accidentally relies on prior test parser state.
        std::fs::write(&path, &case.input).expect("source");
        let mut document = MarkdownDocumentBuffer::open(&path, 1024 * 1024).expect("buffer");
        let before = MarkdownTextSelection {
            anchor_utf16: case.anchor,
            head_utf16: case.head,
        };
        let command = if case.action == "newline" {
            // The oracle already applied the upstream default Enter fallback after Markdown declined.
            MarkdownEditingCommand::InsertNewline
        } else {
            // A declined markup Backspace must preserve source/selection for the native ordinary binding.
            MarkdownEditingCommand::DeleteMarkupBackward
        };
        let result = apply_markup_command(&mut document, command, before);
        let actual = document_text(&document);
        let selection = match &result {
            // Committed command receipts own the final native caret; declined actions preserve caller selection.
            Ok(MarkdownEditingOutcome::Changed(edit)) => edit.selection_after,
            _ => before,
        };
        let handled = result
            .as_ref()
            .is_ok_and(|result| !matches!(result, MarkdownEditingOutcome::NotHandled));
        if handled != case.handled
            || actual != case.text
            || selection.anchor_utf16 != case.anchor_after
            || selection.head_utf16 != case.head_after
        {
            // Report a finite counterexample prefix while still counting all independent oracle failures.
            if reported.insert((case.action.clone(), case.input.clone())) {
                // Report one concrete position per source/action while the total still counts every failure.
                eprintln!(
                    "markup mismatch {} {:?} ({},{}): actual {:?} {:?} {:?}; expected {:?} ({},{}) handled={}",
                    case.action,
                    case.input,
                    case.anchor,
                    case.head,
                    result,
                    actual,
                    selection,
                    case.text,
                    case.anchor_after,
                    case.head_after,
                    case.handled
                );
            }
            mismatches += 1;
        }
    }
    assert_eq!(mismatches, 0, "actual CodeMirror command mismatches");
}

/// Full-string snapshots belong only to small test assertions, never production command planning.
fn document_text(document: &MarkdownDocumentBuffer) -> String {
    let mut units = vec![0; document.utf16_len()];
    document
        .copy_utf16_code_units(0, &mut units)
        .expect("source units");
    String::from_utf16(&units).expect("canonical text")
}

/// A rejected session append budget preserves live text, history/redo, selection and every storage counter.
#[test]
fn exhausted_markup_append_budget_is_atomic() {
    let directory = tempfile::tempdir().expect("budget fixture");
    let path = directory.path().join("note.md");
    std::fs::write(&path, "- a").expect("source");
    let mut document = MarkdownDocumentBuffer::open(&path, 100).expect("buffer");
    for index in 0..98 {
        // Repeated scalar replacements consume the documented lifetime budget without growing live text.
        let value = if index % 2 == 0 { "b" } else { "a" };
        document
            .replace_utf16_range(
                2..3,
                value,
                MarkdownTextSelection {
                    anchor_utf16: 3,
                    head_utf16: 3,
                },
            )
            .expect("seed edit");
    }
    let profile = document.storage_profile();
    let generation = document.generation();
    let selection = document.selection();
    let text = document_text(&document);
    assert!(matches!(
        apply_markup_command(
            &mut document,
            MarkdownEditingCommand::InsertNewline,
            selection
        ),
        Err(MarkdownDocumentBufferError::EditBufferLimitExceeded)
    ));
    assert_eq!(document.storage_profile(), profile);
    assert_eq!(document.generation(), generation);
    assert_eq!(document.selection(), selection);
    assert_eq!(document_text(&document), text);
}

/// Candidate validation after actual append must roll back segments and retain a preexisting redo branch.
#[test]
fn postappend_markup_boundary_failure_rolls_back_every_unpublished_change() {
    let directory = tempfile::tempdir().expect("rollback fixture");
    let path = directory.path().join("note.md");
    std::fs::write(&path, "😀x").expect("source");
    let mut document = MarkdownDocumentBuffer::open(&path, 1024).expect("buffer");
    document
        .replace_utf16_range(
            2..3,
            "y",
            MarkdownTextSelection {
                anchor_utf16: 3,
                head_utf16: 3,
            },
        )
        .expect("seed");
    document.undo().expect("redo branch");
    let profile = document.storage_profile();
    let generation = document.generation();
    let selection = document.selection();
    let line = SourceLine::at(&document, 0).expect("line");
    let mut plan = Plan::new(Vec::new(), line, "\n", &document).expect("plan");
    plan.add(3..3, Payload::Spaces(20));
    // The original emoji stays at 0..2; selecting its surrogate interior fails after candidate bytes append.
    plan.after = 1;
    assert!(transaction::commit(&mut document, selection, plan).is_err());
    assert_eq!(document.storage_profile(), profile);
    assert_eq!(document.generation(), generation);
    assert_eq!(document.selection(), selection);
    assert_eq!(document_text(&document), "😀x");
    assert!(document.can_redo());
}

/// Enter after a giant list line shares its entire original body and records one structural undo state.
#[test]
fn sparse_markup_shares_giant_body_and_preserves_history_selection() {
    let directory = tempfile::tempdir().expect("sparse fixture");
    let path = directory.path().join("note.md");
    let source = format!("- {}", "中".repeat(350_000));
    std::fs::write(&path, &source).expect("source");
    let mut document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).expect("buffer");
    let before = MarkdownTextSelection {
        anchor_utf16: document.utf16_len(),
        head_utf16: document.utf16_len(),
    };
    let original = document.storage_profile();
    let result = apply_markup_command(&mut document, MarkdownEditingCommand::InsertNewline, before)
        .expect("command");
    let MarkdownEditingOutcome::Changed(edit) = result else {
        panic!("Enter must commit");
    };
    assert_eq!(edit.selection_after.head_utf16, before.head_utf16 + 3);
    let profile = document.storage_profile();
    assert_eq!(profile.appended_edit_bytes, 3);
    assert!(profile.piece_count <= original.piece_count + 2);
    assert_eq!(profile.undo_entries, 1);
    assert_eq!(document_text(&document), format!("{source}\n- "));
    assert_eq!(document.undo().expect("undo"), before);
    assert_eq!(document_text(&document), source);
    assert!(!document.is_dirty());
    assert_eq!(document.redo().expect("redo"), edit.selection_after);
}

/// Dense ordered renumbering retains no per-item change list and bounds resulting leaves by canonical chunks.
#[test]
fn dense_ordered_markup_streams_one_replacement_and_one_undo_entry() {
    let directory = tempfile::tempdir().expect("dense fixture");
    let path = directory.path().join("note.md");
    let mut source = String::new();
    let mut expected = String::from("1. item\n2. ");
    for number in 1..=12_000 {
        // Fixture-only strings independently state the ordinary consecutive ordered-list transformation.
        if number > 1 {
            // Following source siblings advance once while the inserted empty item retains its own second marker.
            source.push('\n');
            expected.push('\n');
            expected.push_str(&format!("{}. item", number + 1));
        }
        source.push_str(&format!("{number}. item"));
    }
    std::fs::write(&path, &source).expect("source");
    let mut document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).expect("buffer");
    let before = MarkdownTextSelection {
        anchor_utf16: 7,
        head_utf16: 7,
    };
    let result = apply_markup_command(&mut document, MarkdownEditingCommand::InsertNewline, before)
        .expect("command");
    let MarkdownEditingOutcome::Changed(edit) = result else {
        panic!("ordered Enter must commit");
    };
    assert_eq!(document_text(&document), expected);
    let profile = document.storage_profile();
    assert_eq!(profile.undo_entries, 1);
    assert!(profile.piece_count < expected.len().div_ceil(ORIGINAL_PIECE_BYTES) + 8);
    assert!(profile.appended_edit_bytes <= expected.len());
    assert_eq!(document.undo().expect("undo"), before);
    assert_eq!(document_text(&document), source);
    assert_eq!(document.redo().expect("redo"), edit.selection_after);
    assert_eq!(document_text(&document), expected);
}

/// Backward trimming must revisit a UTF-8 scalar cut by a scratch boundary and preserve physical CRLF.
#[test]
fn trailing_unicode_whitespace_crosses_scratch_and_piece_boundaries() {
    let directory = tempfile::tempdir().expect("tail fixture");
    let path = directory.path().join("note.md");
    for whitespace in [
        " \t".repeat(5000),
        "\u{2003}".repeat(5000),
        "\u{feff}".repeat(5000),
    ] {
        // Distinct UTF-8 widths exercise arbitrary window starts while the emoji body must remain intact.
        let source = format!("- 😀中{whitespace}\r\nother");
        std::fs::write(&path, &source).expect("source");
        let mut document = MarkdownDocumentBuffer::open(&path, 128 * 1024).expect("buffer");
        let position = 5 + whitespace.encode_utf16().count();
        let before = MarkdownTextSelection {
            anchor_utf16: position,
            head_utf16: position,
        };
        apply_markup_command(&mut document, MarkdownEditingCommand::InsertNewline, before)
            .expect("trimmed Enter");
        assert_eq!(document_text(&document), "- 😀中\r\n- \r\nother");
        assert_eq!(document.selection().head_utf16, 9);
        assert_eq!(document.undo(), Some(before));
        assert_eq!(document_text(&document), source);
    }
}

/// Arbitrary nesting retains only the active path and appends its next-line prefix without body copies.
#[test]
fn deep_quote_and_bullet_commands_have_linear_active_metadata() {
    let directory = tempfile::tempdir().expect("deep fixture");
    let path = directory.path().join("note.md");
    let depth = 8192;
    for marker in ["> ", "- "] {
        // A content suffix makes the repeated bullets a nested container chain instead of a thematic break.
        let source = format!("{}body", marker.repeat(depth));
        std::fs::write(&path, &source).expect("source");
        let mut document = MarkdownDocumentBuffer::open(&path, 1024 * 1024).expect("buffer");
        let line = SourceLine::at(&document, 0).expect("line");
        let mut scanner = Scanner::new();
        assert!(!scanner.consume(&document, line).expect("scan"));
        assert_eq!(scanner.contexts.len(), depth);
        assert!(scanner.contexts.capacity() < depth * 2);
        assert!(std::mem::size_of::<Context>() <= 128);
        // Release the diagnostic path before the actual command acquires its own transient descriptors.
        drop(scanner);
        let before = MarkdownTextSelection {
            anchor_utf16: document.utf16_len(),
            head_utf16: document.utf16_len(),
        };
        let previous = document.storage_profile();
        apply_markup_command(&mut document, MarkdownEditingCommand::InsertNewline, before)
            .expect("deep Enter");
        let suffix = if marker == "> " {
            // Quotes repeat every ancestor's visible marker on the generated source line.
            format!("\n{}", marker.repeat(depth))
        } else {
            // Lists blank outer markers into indentation and advance only the innermost item.
            format!("\n{}- ", "  ".repeat(depth - 1))
        };
        assert_eq!(document_text(&document), format!("{source}{suffix}"));
        assert_eq!(document.storage_profile().appended_edit_bytes, suffix.len());
        assert!(
            document.storage_profile().piece_count
                <= previous.piece_count + suffix.len().div_ceil(ORIGINAL_PIECE_BYTES) + 2
        );
        assert_eq!(document.storage_profile().undo_entries, 1);
        assert_eq!(document.undo(), Some(before));
        assert_eq!(document_text(&document), source);
    }
}

/// Opt-in release measurements expose command cost and canonical storage without imposing timing assertions.
#[test]
#[ignore = "explicit 32-sample release resource profile"]
fn profile_markup_command_resources() {
    let directory = tempfile::tempdir().expect("profile fixture");
    let path = directory.path().join("note.md");
    let dense = (1..=50_000)
        .map(|number| {
            // Fixture construction happens before timing; only canonical command execution is measured below.
            format!("{number}. item\n")
        })
        .collect::<String>();
    let cases = [
        ("plain-4MiB", "x".repeat(4 * 1024 * 1024), None),
        (
            "list-body-4MiB",
            format!("- {}", "x".repeat(4 * 1024 * 1024)),
            None,
        ),
        ("ordered-50000", dense, Some(7)),
        ("quotes-8192", format!("{}body", "> ".repeat(8192)), None),
        ("bullets-8192", format!("{}body", "- ".repeat(8192)), None),
    ];
    for (label, source, caret) in cases {
        // Each independent source snapshot is opened outside the measured command interval.
        std::fs::write(&path, source).expect("source");
        let mut samples = Vec::with_capacity(32);
        let mut final_profile = None;
        for _ in 0..32 {
            // A fresh document prevents retained undo/append bytes from contaminating the next sample.
            let mut document =
                MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).expect("buffer");
            let position = caret.unwrap_or(document.utf16_len());
            let before = MarkdownTextSelection {
                anchor_utf16: position,
                head_utf16: position,
            };
            let start = std::time::Instant::now();
            let result =
                apply_markup_command(&mut document, MarkdownEditingCommand::InsertNewline, before)
                    .expect("profile Enter");
            samples.push(start.elapsed().as_nanos());
            assert!(matches!(result, MarkdownEditingOutcome::Changed(_)));
            final_profile = Some(document.storage_profile());
        }
        samples.sort_unstable();
        // Nearest-rank percentiles retain the observed 32nd sample as p99, with no interpolation claim.
        eprintln!(
            "MARKUP_PROFILE {label} n=32 p50_ns={} p95_ns={} p99_ns={} storage={:?}",
            samples[15],
            samples[30],
            samples[31],
            final_profile.expect("samples")
        );
    }
}
