//! Differential behavior and atomic, bounded-storage regressions for formatting transactions.

use super::*;
use serde::Deserialize;
use tempfile::tempdir;

/// Frozen output produced by executing the original formatter with its real CodeMirror dependency.
#[derive(Deserialize)]
struct ReferenceCorpus {
    source_sha256: String,
    codemirror_version: String,
    cases: Vec<ReferenceCase>,
}

/// A synthetic document and one original primary-selection command, including backward orientation.
#[derive(Deserialize)]
struct ReferenceCase {
    action: String,
    input: String,
    anchor: usize,
    head: usize,
    text: String,
    anchor_after: usize,
    head_after: usize,
}

/// Resolve fixture names without extending the public command enum with a serialization contract.
fn action_named(name: &str) -> MarkdownFormatAction {
    use MarkdownFormatAction::*;
    const ACTIONS: &[(&str, MarkdownFormatAction)] = &[
        ("bold", Bold),
        ("italic", Italic),
        ("underline", Underline),
        ("strikethrough", Strikethrough),
        ("inline-code", InlineCode),
        ("inline-formula", InlineFormula),
        ("superscript", Superscript),
        ("link", Link),
        ("heading-1", Heading1),
        ("heading-2", Heading2),
        ("heading-3", Heading3),
        ("bullet-list", BulletList),
        ("ordered-list", OrderedList),
        ("task-list", TaskList),
        ("blockquote", Blockquote),
        ("divider", Divider),
        ("code-block", CodeBlock),
        ("formula-block", FormulaBlock),
        ("table", Table),
    ];
    ACTIONS
        .iter()
        .find_map(|(key, action)| {
            // Only fixture names explicitly corresponding to product commands can drive this comparison.
            (*key == name).then_some(*action)
        })
        .expect("the reference uses a known baseline command")
}

/// Collect synthetic test output only; production formatting never uses a complete-text mirror.
fn text(document: &MarkdownDocumentBuffer) -> String {
    let mut bytes = Vec::new();
    document.write_to(&mut bytes).unwrap();
    String::from_utf8(bytes).unwrap()
}

/// Every command matches the original editor's text and UTF-16 selection over diverse primary ranges.
#[test]
fn matches_original_editor_differential_corpus() {
    // The corpus is generated from unchanged original source, not a second Rust formatting algorithm.
    let corpus: ReferenceCorpus =
        serde_json::from_str(include_str!("reference_cases.json")).unwrap();
    assert_eq!(
        corpus.source_sha256,
        "e30bf8c8b9933e647d5971bcfb6a8760561228164960581de2e120d5587d3746"
    );
    assert_eq!(corpus.codemirror_version, "6.7.1");
    assert_eq!(corpus.cases.len(), 4180);
    let directory = tempdir().unwrap();
    let path = directory.path().join("reference.md");
    for case in corpus.cases {
        // Each independent file starts with no history; no real application data enters the corpus.
        std::fs::write(&path, &case.input).unwrap();
        let mut document = MarkdownDocumentBuffer::open(&path, 4096).unwrap();
        let before = MarkdownTextSelection {
            anchor_utf16: case.anchor,
            head_utf16: case.head,
        };
        let edit = document
            .apply_format(action_named(&case.action), before)
            .unwrap();
        let expected = MarkdownTextSelection {
            anchor_utf16: case.anchor_after,
            head_utf16: case.head_after,
        };
        assert_eq!(
            text(&document),
            case.text,
            "{} {:?} {:?}",
            case.action,
            case.input,
            before
        );
        assert_eq!(
            edit.selection_after, expected,
            "{} {:?} {:?}",
            case.action, case.input, before
        );
        assert_eq!(document.selection(), expected);
        assert_eq!(document.storage_profile().undo_entries, 1);
        // One undo restores both original text and caller-supplied selection, including direction.
        let undo = document.undo_text_edit().unwrap();
        assert_eq!(text(&document), case.input);
        assert_eq!(undo.selection_after, before);
        // Redo exposes aggregate geometry and selection consistent with the original command receipt.
        let redo = document.redo_text_edit().unwrap();
        assert_eq!(text(&document), case.text);
        assert_eq!(redo, edit);
    }
}

/// Formatting a selection beyond the materialization cap retains its body and one history entry.
#[test]
fn large_wrapping_and_multiline_prefixes_store_only_markers() {
    // A half-MiB input exceeds the viewport copy cap while staying below the sparse-line threshold.
    let directory = tempdir().unwrap();
    let path = directory.path().join("large.md");
    let input = format!("{}\n", "a".repeat(255)).repeat(2048);
    std::fs::write(&path, &input).unwrap();
    let mut document = MarkdownDocumentBuffer::open(&path, (input.len() * 2) as u64).unwrap();
    let original_length = document.utf16_len();
    let selection = MarkdownTextSelection {
        anchor_utf16: original_length,
        head_utf16: 0,
    };
    let edit = document
        .apply_format(MarkdownFormatAction::Bold, selection)
        .unwrap();
    assert_eq!(edit.replaced_utf16_range, 0..original_length);
    assert_eq!(edit.change_in_utf16_length, 4);
    assert_eq!(document.storage_profile().appended_edit_bytes, 4);
    assert_eq!(document.storage_profile().undo_entries, 1);
    assert_eq!(document.byte_len(), input.len() + 4);
    assert_eq!(text(&document), format!("**{input}**"));
    assert_eq!(document.undo(), Some(selection));

    // Reuse one immutable prefix at all 2049 lines, including the selected trailing empty line.
    let old_edits = document.storage_profile().appended_edit_bytes;
    let lines = document.line_count();
    let edit = document
        .apply_format(MarkdownFormatAction::BulletList, selection)
        .unwrap();
    assert_eq!(edit.change_in_utf16_length, (lines * 2) as isize);
    assert_eq!(document.byte_len(), input.len() + lines * 2);
    assert_eq!(
        document.storage_profile().appended_edit_bytes - old_edits,
        2
    );
    assert_eq!(document.storage_profile().undo_entries, 1);
    assert_eq!(document.undo(), Some(selection));
    assert_eq!(text(&document), input);
}

/// Failure after one marker append must roll back hidden storage as well as visible editor state.
#[test]
fn edit_budget_failure_rolls_back_partially_prepared_markers() {
    // Eight valid edits consume all but two bytes of a ten-byte lifetime edit budget.
    let directory = tempdir().unwrap();
    let path = directory.path().join("budget.md");
    std::fs::write(&path, "abcdef").unwrap();
    let mut document = MarkdownDocumentBuffer::open(&path, 10).unwrap();
    for _ in 0..8 {
        // Replacing a single byte stays within the live cap while legitimately consuming edit storage.
        document
            .replace_utf16_range(0..1, "x", MarkdownTextSelection::caret(1))
            .unwrap();
    }
    let profile = document.storage_profile();
    let root = document.root.clone();
    let generation = document.generation();
    let selection = document.selection();
    let before = text(&document);
    // The closing marker fits; the opening marker fails after the unpublished candidate already exists.
    assert_eq!(
        document.apply_format(
            MarkdownFormatAction::Bold,
            MarkdownTextSelection {
                anchor_utf16: 1,
                head_utf16: 3
            }
        ),
        Err(MarkdownDocumentBufferError::EditBufferLimitExceeded)
    );
    assert!(piece_tree_links_share_root(&root, &document.root));
    assert_eq!(
        document.storage_profile().appended_edit_bytes,
        profile.appended_edit_bytes
    );
    assert_eq!(
        document.storage_profile().allocated_edit_segment_bytes,
        profile.allocated_edit_segment_bytes
    );
    assert_eq!(
        document.storage_profile().undo_entries,
        profile.undo_entries
    );
    assert_eq!(document.generation(), generation);
    assert_eq!(document.selection(), selection);
    assert_eq!(text(&document), before);
}

/// Invalid native coordinates and an oversized result reject before any resource/history mutation.
#[test]
fn invalid_scalar_and_live_size_preserve_state() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("unicode.md");
    std::fs::write(&path, "😀abc").unwrap();
    let mut document = MarkdownDocumentBuffer::open(&path, 9).unwrap();
    let selection = MarkdownTextSelection::caret(2);
    document.set_selection(selection).unwrap();
    // A low-surrogate interior cannot become the start of a Markdown command.
    assert_eq!(
        document.apply_format(MarkdownFormatAction::Bold, MarkdownTextSelection::caret(1)),
        Err(MarkdownDocumentBufferError::NotUnicodeBoundary)
    );
    // The original seven-byte file fits, but adding four wrapper bytes would exceed nine.
    assert_eq!(
        document.apply_format(
            MarkdownFormatAction::Bold,
            MarkdownTextSelection {
                anchor_utf16: 2,
                head_utf16: 5
            }
        ),
        Err(MarkdownDocumentBufferError::DocumentTooLarge)
    );
    assert_eq!(document.selection(), selection);
    assert_eq!(document.generation(), 0);
    assert_eq!(document.storage_profile().appended_edit_bytes, 0);
    assert!(!document.can_undo());
    assert_eq!(text(&document), "😀abc");
}

/// Native storage preserves CRLF/CR bytes even when heading and selected-line markers change.
#[test]
fn formatting_does_not_normalize_existing_line_endings() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("crlf.md");
    std::fs::write(&path, "###\t title\r\nsecond\rthird").unwrap();
    let mut document = MarkdownDocumentBuffer::open(&path, 4096).unwrap();
    // Only the head's existing prefix is replaced; its CRLF remains a shared original piece.
    document
        .apply_format(
            MarkdownFormatAction::Heading1,
            MarkdownTextSelection::caret(5),
        )
        .unwrap();
    assert_eq!(text(&document), "# title\r\nsecond\rthird");
    let end = document.utf16_len();
    document
        .apply_format(
            MarkdownFormatAction::Blockquote,
            MarkdownTextSelection {
                anchor_utf16: 0,
                head_utf16: end,
            },
        )
        .unwrap();
    assert_eq!(text(&document), "> # title\r\n> second\r> third");
}

/// Million-line formatting retains chunk-scale canonical metadata rather than one leaf per line.
#[test]
fn dense_newlines_stream_canonical_bytes_and_keep_tree_metadata_bounded() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("dense.md");
    let input = "\n".repeat(1024 * 1024);
    std::fs::write(&path, &input).unwrap();
    let mut document = MarkdownDocumentBuffer::open(&path, (input.len() * 4) as u64).unwrap();
    let before = document.storage_profile();
    let selection = MarkdownTextSelection {
        anchor_utf16: input.len(),
        head_utf16: 0,
    };
    let edit = document
        .apply_format(MarkdownFormatAction::BulletList, selection)
        .unwrap();
    let expected = format!("{}- ", "- \n".repeat(input.len()));
    let profile = document.storage_profile();
    assert_eq!(text(&document), expected);
    assert_eq!(profile.appended_edit_bytes, expected.len());
    assert!(profile.allocated_edit_segment_bytes <= expected.len() + EDIT_SEGMENT_BYTES);
    // Each fixed-scratch flush yields at most two leaves at a segment boundary, plus a shared suffix.
    assert!(profile.piece_count <= expected.len().div_ceil(ORIGINAL_PIECE_BYTES) * 2 + 2);
    assert_eq!(profile.undo_entries, 1);
    assert_eq!(document.line_count(), input.len() + 1);
    assert_eq!(document.character_count(), expected.len());
    // Bullet markers and whitespace contain no Unicode letters or numbers, even across a million lines.
    assert_eq!(document.word_count(), 0);
    assert_eq!(
        edit.change_in_utf16_length,
        ((input.len() + 1) * 2) as isize
    );
    // The old canonical root remains shared for one-step undo, and redo restores the exact aggregate edit.
    assert_eq!(document.undo(), Some(selection));
    assert_eq!(document.storage_profile().piece_count, before.piece_count);
    assert_eq!(text(&document), input);
    assert_eq!(document.redo_text_edit(), Some(edit));
    assert_eq!(text(&document), expected);
}

/// Dense streaming preserves split CRLF, Unicode scalars, and late existing edit-backed source pieces.
#[test]
fn dense_stream_preserves_line_endings_and_existing_edit_piece_sources() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("dense-unicode.md");
    // The first CR ends a 4-KiB original leaf; its LF belongs to the following source piece.
    let input = format!("{}\r\n{}tail", "a".repeat(4095), "β😀\r\n".repeat(35000));
    std::fs::write(&path, &input).unwrap();
    let mut document = MarkdownDocumentBuffer::open(&path, (input.len() * 4) as u64).unwrap();
    let edited_byte = input.rfind('β').unwrap();
    let edited_utf16 = input[..edited_byte].encode_utf16().count();
    // This source piece is reached only after streaming enough output to grow the segment vector.
    document
        .replace_utf16_range(
            edited_utf16..edited_utf16 + 1,
            "γ",
            MarkdownTextSelection::caret(edited_utf16 + 1),
        )
        .unwrap();
    let before_format = text(&document);
    let selection = MarkdownTextSelection {
        anchor_utf16: document.utf16_len(),
        head_utf16: 0,
    };
    let edit = document
        .apply_format(MarkdownFormatAction::Blockquote, selection)
        .unwrap();
    let expected = format!(
        "> {}\r\n{}> γ😀\r\n> tail",
        "a".repeat(4095),
        "> β😀\r\n".repeat(34999)
    );
    assert_eq!(text(&document), expected);
    assert_eq!(document.utf16_len(), expected.encode_utf16().count());
    assert_eq!(document.byte_len(), expected.len());
    assert_eq!(document.line_count(), 35002);
    assert_eq!(document.character_count(), expected.chars().count() - 35001);
    assert_eq!(edit.change_in_utf16_length, (35002 * 2) as isize);
    assert_eq!(document.undo(), Some(selection));
    assert_eq!(text(&document), before_format);
    assert_eq!(document.redo_text_edit(), Some(edit));
    assert_eq!(text(&document), expected);
}

/// A dense command preflights its complete canonical append, preserving root/history/redo on failure.
#[test]
fn dense_edit_budget_failure_preflights_before_any_append() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("dense-budget.md");
    std::fs::write(&path, format!("x{}", "\n".repeat(5000))).unwrap();
    let mut document = MarkdownDocumentBuffer::open(&path, 16000).unwrap();
    // Two legal edits consume append budget without increasing the final live byte count.
    document
        .replace_utf16_range(0..1, &"x".repeat(2000), MarkdownTextSelection::caret(2000))
        .unwrap();
    document
        .replace_utf16_range(0..2000, "x", MarkdownTextSelection::caret(1))
        .unwrap();
    document
        .replace_utf16_range(0..1, "y", MarkdownTextSelection::caret(1))
        .unwrap();
    document.undo_text_edit().unwrap();
    let root = document.root.clone();
    let profile = document.storage_profile();
    let generation = document.generation();
    let selection = document.selection();
    let before = text(&document);
    let all = MarkdownTextSelection {
        anchor_utf16: 0,
        head_utf16: document.utf16_len(),
    };
    // Logical output fits the live cap, but its new canonical copy exceeds the remaining append budget.
    assert_eq!(
        document.apply_format(MarkdownFormatAction::BulletList, all),
        Err(MarkdownDocumentBufferError::EditBufferLimitExceeded)
    );
    assert!(piece_tree_links_share_root(&root, &document.root));
    assert_eq!(document.storage_profile(), profile);
    assert_eq!(document.generation(), generation);
    assert_eq!(document.selection(), selection);
    assert_eq!(text(&document), before);
    assert!(document.can_redo());
}

/// A long Unicode-whitespace heading is scanned once and remains one structural undo operation.
#[test]
fn long_heading_whitespace_is_replaced_without_materialization() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("long-heading.md");
    let whitespace = " \t\u{a0}\u{feff}".repeat(65536);
    let input = format!("#{whitespace}body\r\nnext");
    std::fs::write(&path, &input).unwrap();
    let mut document = MarkdownDocumentBuffer::open(&path, (input.len() * 2) as u64).unwrap();
    let caret = MarkdownTextSelection::caret(1 + whitespace.encode_utf16().count() + 4);
    let edit = document
        .apply_format(MarkdownFormatAction::Heading2, caret)
        .unwrap();
    assert_eq!(text(&document), "## body\r\nnext");
    assert_eq!(edit.selection_after, MarkdownTextSelection::caret(7));
    assert_eq!(document.storage_profile().appended_edit_bytes, 3);
    assert_eq!(document.storage_profile().undo_entries, 1);
    assert_eq!(document.undo(), Some(caret));
    assert_eq!(text(&document), input);
}

/// Dense subranges retain outside pieces and map backward endpoint association across LF and CR.
#[test]
fn dense_partial_lines_preserve_outside_text_and_backward_selection() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("dense-subrange.md");
    for separator in ['\n', '\r'] {
        // Both logical separator spellings must insert one marker per line without normalization.
        let selected = separator.to_string().repeat(5000);
        let input = format!("start\n{selected}tail");
        std::fs::write(&path, &input).unwrap();
        let mut document = MarkdownDocumentBuffer::open(&path, 32768).unwrap();
        let before = MarkdownTextSelection {
            anchor_utf16: document.utf16_len(),
            head_utf16: 6,
        };
        let edit = document
            .apply_format(MarkdownFormatAction::Blockquote, before)
            .unwrap();
        let expected = format!("start\n{}> tail", format!("> {separator}").repeat(5000));
        assert_eq!(text(&document), expected);
        assert_eq!(edit.replaced_utf16_range, 6..input.len() - 4);
        assert_eq!(
            edit.selection_after,
            MarkdownTextSelection {
                anchor_utf16: expected.len(),
                head_utf16: 8
            }
        );
        // The first line and last body stay shared; only the affected line starts/contents are appended.
        assert_eq!(
            document.storage_profile().appended_edit_bytes,
            5000 + 5001 * 2
        );
        assert_eq!(document.undo(), Some(before));
        assert_eq!(text(&document), input);
    }
}

/// Report warm synthetic formatting distributions and actual retained canonical storage counters.
#[test]
#[ignore = "explicit release-only synthetic resource and latency profile"]
fn profile_dense_and_long_heading_formatting() {
    use std::time::Instant;
    const SAMPLES: usize = 32;
    let directory = tempdir().unwrap();
    for bytes in [1024 * 1024, 4 * 1024 * 1024] {
        // Each size isolates formatting after fresh opens of a synthetic file already in the cache.
        for heading in [false, true] {
            // Dense newlines stress insertion count; long heading whitespace stresses ordered scanning.
            let path = directory.path().join("profile.md");
            let input = if heading {
                // ASCII whitespace gives a reproducible removable prefix spanning many original pieces.
                format!("#{}body", " ".repeat(bytes))
            } else {
                // Every byte is a logical line, maximizing the former sparse algorithm's metadata risk.
                "\n".repeat(bytes)
            };
            std::fs::write(&path, &input).unwrap();
            let mut elapsed_ms = Vec::with_capacity(SAMPLES);
            let mut last_profile = None;
            let mut old_pieces = 0;
            for _ in 0..SAMPLES {
                // File snapshot/open work stays outside the formatting clock; every sample has fresh history.
                let mut document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap();
                old_pieces = document.storage_profile().piece_count;
                let selection = MarkdownTextSelection {
                    anchor_utf16: 0,
                    head_utf16: document.utf16_len(),
                };
                let action = if heading {
                    // The head at EOF still targets the sole line, removing all heading whitespace.
                    MarkdownFormatAction::Heading1
                } else {
                    // Include all newline starts and the final empty endpoint line in the dense path.
                    MarkdownFormatAction::BulletList
                };
                let start = Instant::now();
                document.apply_format(action, selection).unwrap();
                elapsed_ms.push(start.elapsed().as_secs_f64() * 1000.0);
                let profile = document.storage_profile();
                if heading {
                    // Only the fixed marker is appended; the unchanged body suffix stays in original storage.
                    assert_eq!(profile.appended_edit_bytes, 2);
                    assert_eq!(text(&document), "# body");
                } else {
                    // Chunk-scale leaves bound metadata even for millions of lines.
                    assert_eq!(profile.appended_edit_bytes, bytes * 3 + 2);
                    assert!(
                        profile.piece_count
                            <= profile.appended_edit_bytes.div_ceil(ORIGINAL_PIECE_BYTES) * 2 + 2
                    );
                }
                last_profile = Some(profile);
            }
            elapsed_ms.sort_by(f64::total_cmp);
            let percentile = |percent: usize| {
                // Nearest-rank quantiles are explicit; 32 samples make p99 the maximum observed sample.
                elapsed_ms[(SAMPLES * percent).div_ceil(100).saturating_sub(1)]
            };
            let profile = last_profile.expect("the fixed nonempty sample loop produced a profile");
            // A binary tree has at most 2*leaves-1 logical nodes. This upper bound includes Arc counters,
            // excludes allocator bookkeeping and prior undo roots, and decreases when nodes are shared.
            let node_bytes =
                std::mem::size_of::<MarkdownPieceTreeNode>() + 2 * std::mem::size_of::<usize>();
            let tree_metadata_upper_bound = profile
                .piece_count
                .saturating_mul(2)
                .saturating_sub(1)
                .saturating_mul(node_bytes);
            println!(
                "format_profile heading={heading} samples={SAMPLES} input_bytes={} p50_ms={:.3} p95_ms={:.3} p99_ms={:.3} original_pieces={old_pieces} current_pieces={} appended_bytes={} edit_segment_capacity={} current_tree_metadata_upper_bound_bytes={tree_metadata_upper_bound}",
                input.len(),
                percentile(50),
                percentile(95),
                percentile(99),
                profile.piece_count,
                profile.appended_edit_bytes,
                profile.allocated_edit_segment_bytes
            );
        }
    }
}
