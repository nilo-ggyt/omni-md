//! Differential source-coordinate fixtures and bounded-storage folding regressions.

use super::*;
use serde::Deserialize;

/// The reference is generated from the preserved product's CodeMirror Markdown fold service.
#[derive(Deserialize)]
struct Reference {
    source: String,
    line: usize,
    expected: Option<[usize; 2]>,
}

/// Verify each frozen actual editor query without normalizing stored source coordinates.
#[test]
fn preserved_editor_fold_ranges_match_native_source_coordinates() {
    let references: Vec<Reference> =
        serde_json::from_str(include_str!("reference.json")).expect("fold references");
    for reference in references {
        // Each independent fixture owns its immutable source snapshot until the complete query returns.
        let directory = tempfile::tempdir().expect("fold fixture");
        let path = directory.path().join("note.md");
        std::fs::write(&path, &reference.source).expect("source");
        let document = MarkdownDocumentBuffer::open(&path, (reference.source.len() + 1024) as u64)
            .expect("buffer");
        let actual = markdown_fold_region(&document, reference.line)
            .expect("fold query")
            .map(|region| {
                // Compare both canonical endpoints independently of the native presentation wrapper.
                [
                    region.hidden_utf16_range.start,
                    region.hidden_utf16_range.end,
                ]
            });
        assert_eq!(
            actual, reference.expected,
            "line {} in {:?}",
            reference.line, reference.source
        );
    }
}

/// Check large-source section scans add no edit/history storage or generation changes.
#[test]
fn giant_line_and_deep_section_queries_preserve_the_canonical_document() {
    let directory = tempfile::tempdir().expect("bounded fixture");
    let path = directory.path().join("large.md");
    let mut source = String::from("# Header 😀\r\n");
    source.push_str(&"中".repeat(350_000));
    source.push_str("\r\n\r\n");
    for _ in 0..2048 {
        // Deep source structure increases scan work without creating a retained syntax-node stack.
        source.push_str("###### Nested\r\nbody\r\n");
    }
    std::fs::write(&path, &source).expect("source");
    let document =
        MarkdownDocumentBuffer::open(&path, (source.len() + 1024) as u64).expect("buffer");
    let profile = document.storage_profile();
    let generation = document.generation();
    let fold = markdown_fold_region(&document, 0)
        .expect("fold query")
        .expect("header section");
    assert_eq!(
        fold.hidden_utf16_range.start,
        "# Header 😀".encode_utf16().count()
    );
    assert_eq!(
        fold.hidden_utf16_range.end,
        source.trim_end_matches(['\r', '\n']).encode_utf16().count()
    );
    assert_eq!(document.generation(), generation);
    assert_eq!(document.storage_profile(), profile);
    assert!(!document.is_dirty());
}

/// Keep folding aligned with edited canonical pieces and reject invalid logical line requests.
#[test]
fn fold_queries_follow_piece_edits_and_reject_outside_lines() {
    let directory = tempfile::tempdir().expect("edited fixture");
    let path = directory.path().join("edit.md");
    std::fs::write(&path, "# H\nfirst\n\n# Next\nsecond").expect("source");
    let mut document = MarkdownDocumentBuffer::open(&path, 4096).expect("buffer");
    document
        .replace_utf16_range(
            4..9,
            "😀 new",
            crate::MarkdownTextSelection {
                anchor_utf16: 10,
                head_utf16: 10,
            },
        )
        .expect("piece edit");
    assert_eq!(
        markdown_fold_region(&document, 0)
            .expect("fold")
            .expect("section")
            .hidden_utf16_range,
        3..10
    );
    assert!(markdown_fold_region(&document, document.line_count()).is_err());
    assert!(markdown_line_may_fold(&document, 0).expect("hint"));
}

/// Reject truncated delimiter authority for distant run, close-tail and opening-info differences.
#[test]
fn complete_long_fence_metadata_preserves_opaque_context_and_exact_closes() {
    let directory = tempfile::tempdir().expect("long fence fixture");
    let path = directory.path().join("fence.md");
    let opening = "`".repeat(PREFIX_UNITS + 1);
    let short_close = "`".repeat(PREFIX_UNITS);
    let source = format!("{opening}\ninside\n{short_close}\n# still code\nbody");
    std::fs::write(&path, &source).expect("source");
    let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).expect("buffer");
    let profile = document.storage_profile();
    let fold = markdown_fold_region(&document, 0)
        .expect("query")
        .expect("unclosed fence");
    assert_eq!(fold.hidden_utf16_range, opening.len()..source.len());
    assert_eq!(
        markdown_fold_region(&document, 3).expect("opaque query"),
        None
    );
    assert_eq!(document.storage_profile(), profile);

    // A trailing non-whitespace scalar beyond the prefix cap prevents an otherwise matching close.
    let source = format!(
        "```\ninside\n```{}x\n# still code\n```\n# outside\nbody",
        " ".repeat(4096)
    );
    std::fs::write(&path, &source).expect("new source");
    let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).expect("new buffer");
    assert_eq!(
        markdown_fold_region(&document, 3).expect("opaque query"),
        None
    );
    let close_end = source.find("\n# outside").expect("exact close");
    assert_eq!(
        markdown_fold_region(&document, 0)
            .expect("query")
            .expect("fence")
            .hidden_utf16_range,
        3..close_end
    );

    // A backtick in distant info text invalidates the opener instead of fabricating an opaque fence.
    let source = format!("```{}\u{0060}\nparagraph\n```\ncode", "a".repeat(4096));
    std::fs::write(&path, &source).expect("invalid info source");
    let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).expect("info buffer");
    assert_eq!(
        markdown_fold_region(&document, 0)
            .expect("paragraph query")
            .expect("paragraph")
            .hidden_utf16_range
            .end,
        source.find("\n```").expect("later valid opener")
    );
}

/// Keep known raw HTML opaque across blanks and refuse uncertain long-tag hiding authority.
#[test]
fn long_html_context_remains_opaque_and_uncertain_tags_grant_no_hiding_authority() {
    let directory = tempfile::tempdir().expect("HTML fixture");
    let path = directory.path().join("html.md");
    let source = format!(
        "<SCRIPT {}>\n\n# still html\n</SCRIPT>\n# Outside\nbody",
        "a".repeat(4096)
    );
    std::fs::write(&path, &source).expect("raw source");
    let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).expect("raw buffer");
    assert_eq!(
        markdown_fold_region(&document, 2).expect("raw interior"),
        None
    );
    assert_eq!(
        markdown_fold_region(&document, 0)
            .expect("raw query")
            .expect("raw block")
            .hidden_utf16_range
            .end,
        source.find("\n# Outside").expect("outside boundary")
    );

    // A truncated custom-tag candidate stays conservative until a blank, while exact source is unchanged.
    let source = format!(
        "<{}\n# uncertain child\nbody\n\n# Outside\nbody",
        "custom".repeat(1024)
    );
    std::fs::write(&path, &source).expect("uncertain source");
    let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).expect("uncertain buffer");
    let profile = document.storage_profile();
    assert_eq!(
        markdown_fold_region(&document, 0).expect("uncertain opener"),
        None
    );
    assert_eq!(
        markdown_fold_region(&document, 1).expect("uncertain interior"),
        None
    );
    assert!(
        markdown_fold_region(&document, 4)
            .expect("outside query")
            .is_some()
    );
    assert_eq!(document.storage_profile(), profile);
    assert!(!document.is_dirty());
}

/// Compare sequential and indexed native line boundaries across mixed endings, scalars and pieces.
#[test]
fn sequential_lines_preserve_crlf_and_surrogate_offsets_across_fixed_chunks_and_pieces() {
    let directory = tempfile::tempdir().expect("cursor fixture");
    let path = directory.path().join("cursor.md");
    let mut source = "a".repeat(PREFIX_UNITS - 1);
    source.push_str("\r\n😀\rstandalone\n\r\nend\r");
    std::fs::write(&path, &source).expect("source");
    let mut document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).expect("buffer");
    document
        .replace_utf16_range(0..1, "中", crate::MarkdownTextSelection::default())
        .expect("piece edit");
    let mut cursor = LineCursor::new(&document, 0, document.line_count());
    for number in 0..document.line_count() {
        // Independent aggregate lookup and sequential decoding must agree on every canonical boundary.
        let expected = Line::read(&document, number).expect("indexed record");
        let actual = cursor
            .next_line()
            .expect("sequential record")
            .expect("line");
        assert_eq!(
            (actual.start, actual.end, actual.prefix, actual.blank),
            (
                expected.start,
                expected.end,
                expected.prefix,
                expected.blank
            )
        );
    }
    assert!(cursor.next_line().expect("exhausted cursor").is_none());
}

/// Suppress obvious duplicate controls using finite local context only.
#[test]
fn gutter_hints_reject_obvious_continuations_without_authorizing_ambiguous_source() {
    let directory = tempfile::tempdir().expect("hint fixture");
    let path = directory.path().join("hints.md");
    std::fs::write(&path, "paragraph\ncontinued\nthird\n\n    code\n    more\n    last\n\n> quote\n> continued\n> last\n\n# Heading\nbody").expect("source");
    let document = MarkdownDocumentBuffer::open(&path, 4096).expect("buffer");
    for line in [1, 2, 5, 6, 9, 10] {
        // The finite previous prefix detects only obvious continuation; it never scans prior fence context.
        assert!(!markdown_line_may_fold(&document, line).expect("hint"));
    }
    assert!(markdown_line_may_fold(&document, 0).expect("paragraph hint"));
    assert!(markdown_line_may_fold(&document, 12).expect("heading hint"));
}

/// Exercise dense prefix scans and giant paint hints without retained canonical storage growth.
#[test]
fn dense_prefix_queries_and_giant_hint_lines_keep_document_storage_unchanged() {
    let directory = tempfile::tempdir().expect("dense fixture");
    let path = directory.path().join("dense.md");
    let mut source = "\n".repeat(131_072);
    source.push_str("# Tail 😀\r\nbody\r\nsecond");
    std::fs::write(&path, &source).expect("dense source");
    let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).expect("dense buffer");
    let profile = document.storage_profile();
    let fold = markdown_fold_region(&document, 131_072)
        .expect("dense query")
        .expect("tail fold");
    assert_eq!(
        fold.hidden_utf16_range.start,
        131_072 + "# Tail 😀".encode_utf16().count()
    );
    assert_eq!(fold.hidden_utf16_range.end, source.encode_utf16().count());
    assert_eq!(document.storage_profile(), profile);

    // Viewport hinting reads finite prefixes even when fence-like info text occupies megabytes.
    let source = format!("```{}\nbody\n```", "a".repeat(4 * 1024 * 1024));
    std::fs::write(&path, &source).expect("giant hint source");
    let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).expect("hint buffer");
    let profile = document.storage_profile();
    assert!(markdown_line_may_fold(&document, 0).expect("giant hint"));
    assert_eq!(document.storage_profile(), profile);
}

/// Measure explicit source work separately from finite paint hints using an opt-in pure release profile.
#[test]
#[ignore = "release-only explicit-query timing evidence; no GUI or persistent benchmark state"]
fn profile_dense_prefix_and_giant_fence_queries() {
    let directory = tempfile::tempdir().expect("profile fixture");
    let path = directory.path().join("profile.md");
    for (name, source, line, hint) in [
        (
            "dense-million-prefix",
            format!("{}# Tail\nbody\nsecond", "\n".repeat(1_048_576)),
            1_048_576,
            false,
        ),
        (
            "giant-4mib-fence-hint",
            format!("```{}\nbody\n```", "a".repeat(4 * 1024 * 1024)),
            0,
            true,
        ),
        (
            "giant-4mib-fence-query",
            format!("```{}\nbody\n```", "a".repeat(4 * 1024 * 1024)),
            0,
            false,
        ),
    ] {
        // Fixture construction and immutable snapshot acquisition remain outside measured query time.
        std::fs::write(&path, &source).expect("source");
        let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).expect("buffer");
        let profile = document.storage_profile();
        let mut samples = Vec::with_capacity(32);
        for _ in 0..32 {
            // Each sample owns only finite query scratch; canonical tree/history storage must stay identical.
            let start = std::time::Instant::now();
            if hint {
                // The paint-time path never scans the complete giant info string.
                assert!(markdown_line_may_fold(&document, line).expect("hint"));
            } else {
                // Only an explicit toggle pays source-linear context/delimiter classification work.
                assert!(
                    markdown_fold_region(&document, line)
                        .expect("query")
                        .is_some()
                );
            }
            samples.push(start.elapsed());
        }
        samples.sort_unstable();
        eprintln!(
            "fold-profile {name}: p50={:?} p95={:?} p99={:?}; storage={profile:?}",
            samples[15], samples[30], samples[31]
        );
        assert_eq!(document.storage_profile(), profile);
    }
}

/// Derive exact canonical CRLF endpoints from the actual LF editor corpus, including non-BMP text.
#[test]
fn lf_reference_ranges_map_to_crlf_without_losing_native_source_units() {
    let references: Vec<Reference> =
        serde_json::from_str(include_str!("reference.json")).expect("references");
    let directory = tempfile::tempdir().expect("CRLF fixture");
    let path = directory.path().join("crlf.md");
    for reference in references {
        // Existing mixed-ending fixtures already encode canonical offsets and must not be normalized again.
        if reference.source.contains('\r') {
            // This property transforms only LF-only reference inputs, preserving their actual editor grammar.
            continue;
        }
        let expected = reference.expected.map(|range| {
            // Each LF before a reference endpoint adds exactly one canonical CR source unit.
            range.map(|offset| {
                // Count native units, not UTF-8 bytes, so emoji and supplementary scalars remain aligned.
                offset
                    + reference
                        .source
                        .encode_utf16()
                        .take(offset)
                        .filter(|unit| *unit == 10)
                        .count()
            })
        });
        let source = reference.source.replace('\n', "\r\n");
        std::fs::write(&path, &source).expect("source");
        let document =
            MarkdownDocumentBuffer::open(&path, (source.len() + 1024) as u64).expect("buffer");
        let actual = markdown_fold_region(&document, reference.line)
            .expect("query")
            .map(|region| {
                // Both endpoints continue to address the complete canonical CRLF source, including hidden text.
                [
                    region.hidden_utf16_range.start,
                    region.hidden_utf16_range.end,
                ]
            });
        assert_eq!(
            actual, expected,
            "CRLF line {} in {:?}",
            reference.line, reference.source
        );
    }
}
