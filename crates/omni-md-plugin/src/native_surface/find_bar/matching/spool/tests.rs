//! Exercise outer template borrows, actual anonymous range records, and atomic
//! transaction failure without starting NSApplication.
use super::*;

fn found(range: Range<usize>, captures: Vec<Option<Range<usize>>>) -> Found {
    Found {
        range,
        precise: true,
        captures,
    }
}

#[test]
fn anonymous_spool_streams_literals_and_source_ranges() {
    // The template outlives its reader. Literals borrow that owner; captures retain
    // only original-snapshot coordinates, including UTF-16 emoji widths.
    let template = ReplacementTemplate::parse("😀$1", true, 2);
    let mut spool = ReplacementSpool::new(&template).unwrap();
    spool
        .push(&found(1..3, vec![Some(1..3), Some(10..12)]))
        .unwrap();
    assert_eq!(spool.count(), 1);
    assert_eq!(spool.inserted_utf16(), 4);
    let mut replacements = spool.into_replacements().unwrap();
    let replacement = replacements.next().unwrap().unwrap();
    assert_eq!(replacement.range_utf16, 1..3);
    let parts = replacement.parts.collect::<Result<Vec<_>, _>>().unwrap();
    assert!(matches!(
        &parts[0],
        MarkdownReplacementPart::Literal(Cow::Borrowed("😀"))
    ));
    assert_eq!(parts[1], MarkdownReplacementPart::SourceRange(10..12));
    assert!(replacements.next().is_none());
}

#[test]
fn unread_parts_cannot_be_reinterpreted_as_another_replacement() {
    // Advancing the outer iterator with unconsumed literal parts must fail; one
    // shared capture scratch cannot masquerade as independent record snapshots.
    let template = ReplacementTemplate::parse("x", false, 0);
    let mut spool = ReplacementSpool::new(&template).unwrap();
    spool.push(&found(0..1, vec![])).unwrap();
    spool.push(&found(2..3, vec![])).unwrap();
    let mut replacements = spool.into_replacements().unwrap();
    let first = replacements.next().unwrap().unwrap();
    assert!(matches!(
        replacements.next(),
        Some(Err(
            MarkdownDocumentBufferError::ReplacementInputUnavailable
        ))
    ));
    drop(first);
    assert!(replacements.next().is_none());
}

#[test]
fn selection_mapping_keeps_reversed_anchor_and_utf16_widths() {
    // Different lengths for the same capture token accumulate in original
    // coordinates without swapping a backward selection's anchor/head.
    let template = ReplacementTemplate::parse("$1", true, 2);
    let mut spool = ReplacementSpool::new(&template).unwrap();
    spool
        .push(&found(1..3, vec![Some(1..3), Some(10..11)]))
        .unwrap();
    spool
        .push(&found(5..6, vec![Some(5..6), Some(10..13)]))
        .unwrap();
    let selection = MarkdownTextSelection {
        anchor_utf16: 8,
        head_utf16: 2,
    };
    assert_eq!(
        spool.map_selection(selection).unwrap(),
        MarkdownTextSelection {
            anchor_utf16: 9,
            head_utf16: 2
        }
    );
    let ranges: Vec<_> = spool
        .into_replacements()
        .unwrap()
        .map(|replacement| {
            // After mapping's read-ahead, rewind to the complete record header
            // rather than inheriting the buffered reader's underlying offset.
            let replacement = replacement.unwrap();
            let range = replacement.range_utf16;
            replacement.parts.collect::<Result<Vec<_>, _>>().unwrap();
            range
        })
        .collect();
    assert_eq!(ranges, [1..3, 5..6]);
}

#[test]
fn missing_capture_rejects_before_canonical_mutation() {
    // A missing required capture slot is malformed input, distinct from a valid
    // nonparticipating branch whose explicit None expands to "undefined".
    let capture = ReplacementTemplate::parse("$1", true, 2);
    let mut incomplete = ReplacementSpool::new(&capture).unwrap();
    assert!(incomplete.push(&found(0..1, vec![])).is_err());
    assert_eq!(incomplete.count(), 0);
}

#[test]
fn truncated_second_record_rolls_back_canonical_text_and_redo() {
    // Truncate the actual second record after the first can be consumed. Core
    // must preserve original text, selection, generation, and redo atomically.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("Spool.md");
    std::fs::write(&path, "abc def").unwrap();
    let mut document = omni_markdown_core::MarkdownDocumentBuffer::open(&path, 4096).unwrap();
    document
        .replace_utf16_range(0..1, "Z", MarkdownTextSelection::caret(1))
        .unwrap();
    document.undo_text_edit().unwrap();
    let generation = document.generation();
    let selection = document.selection();
    let history = (
        document.can_undo(),
        document.can_redo(),
        document.is_dirty(),
    );
    let template = ReplacementTemplate::parse("replacement", false, 0);
    let mut spool = ReplacementSpool::new(&template).unwrap();
    spool.push(&found(0..1, vec![])).unwrap();
    spool.push(&found(4..5, vec![])).unwrap();
    let stream = spool.into_replacements().unwrap();
    let bytes = stream
        .shared
        .state
        .borrow()
        .file
        .get_ref()
        .metadata()
        .unwrap()
        .len();
    stream
        .shared
        .state
        .borrow()
        .file
        .get_ref()
        .set_len(bytes - 1)
        .unwrap();
    assert!(
        document
            .apply_replacements(stream, selection, MarkdownTextSelection::caret(0))
            .is_err()
    );
    let mut actual = Vec::new();
    document.write_to(&mut actual).unwrap();
    assert_eq!(actual, b"abc def");
    assert_eq!(document.generation(), generation);
    assert_eq!(document.selection(), selection);
    assert_eq!(
        (
            document.can_undo(),
            document.can_redo(),
            document.is_dirty()
        ),
        history
    );
    document.redo_text_edit().unwrap();
    let mut redone = Vec::new();
    document.write_to(&mut redone).unwrap();
    assert_eq!(redone, b"Zbc def");
}

#[test]
fn anonymous_spool_replays_captures_as_one_reversible_edit() {
    // Both $1 references read the same old root; preparing the first replacement
    // cannot change the second replacement's captured source text.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("Capture.md");
    std::fs::write(&path, "A😀 B42").unwrap();
    let mut document = omni_markdown_core::MarkdownDocumentBuffer::open(&path, 4096).unwrap();
    let template = ReplacementTemplate::parse("$1", true, 2);
    let mut spool = ReplacementSpool::new(&template).unwrap();
    spool
        .push(&found(0..3, vec![Some(0..3), Some(5..7)]))
        .unwrap();
    spool
        .push(&found(4..7, vec![Some(4..7), Some(1..3)]))
        .unwrap();
    // Native Find must pass the original directed memento, not reconstruct a
    // forward selection from the query's ordered range.
    let before = MarkdownTextSelection {
        anchor_utf16: 7,
        head_utf16: 1,
    };
    let after = spool.map_selection(before).unwrap();
    assert_eq!(
        after,
        MarkdownTextSelection {
            anchor_utf16: 5,
            head_utf16: 2,
        }
    );
    assert!(
        document
            .apply_replacements(spool.into_replacements().unwrap(), before, after)
            .unwrap()
            .is_some()
    );
    let text = |document: &omni_markdown_core::MarkdownDocumentBuffer| {
        let mut bytes = Vec::new();
        document.write_to(&mut bytes).unwrap();
        String::from_utf8(bytes).unwrap()
    };
    assert_eq!(text(&document), "42 😀");
    assert_eq!(document.selection(), after);
    document.undo_text_edit().unwrap();
    assert_eq!(text(&document), "A😀 B42");
    assert_eq!(document.selection(), before);
    document.redo_text_edit().unwrap();
    assert_eq!(text(&document), "42 😀");
    assert_eq!(document.selection(), after);
}

#[test]
fn repeated_capture_tokens_share_one_wire_slot_and_empty_captures_stay_empty() {
    // Repeated tokens share the recorded capture. An absent branch and an empty
    // range must still produce different replacement parts.
    let template = ReplacementTemplate::parse("$1/$1/$2/$3", true, 4);
    assert_eq!(template.captures, [1, 2, 3]);
    let mut spool = ReplacementSpool::new(&template).unwrap();
    spool
        .push(&found(0..3, vec![Some(0..3), Some(1..2), None, Some(3..3)]))
        .unwrap();
    assert_eq!(spool.bytes, 16 + 17 + 1 + 17);
    assert_eq!(spool.inserted_utf16(), 14);
    let mut records = spool.into_replacements().unwrap();
    let parts = records
        .next()
        .unwrap()
        .unwrap()
        .parts
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(parts.iter().filter(|part| matches!(part, MarkdownReplacementPart::SourceRange(range) if *range == (1..2))).count(), 2);
    assert!(parts.iter().any(|part| matches!(
        part,
        MarkdownReplacementPart::Literal(Cow::Borrowed("undefined"))
    )));
    assert!(parts.iter().any(
        |part| matches!(part, MarkdownReplacementPart::SourceRange(range) if *range == (3..3))
    ));
}

#[test]
fn corrupted_capture_tag_cannot_be_accepted_as_a_missing_capture() {
    // Corrupt the actual record's tag. An undefined tag is not a valid absent
    // capture; reject without resynchronizing, ignoring corruption, or emitting a replacement.
    let template = ReplacementTemplate::parse("$1", true, 2);
    let mut spool = ReplacementSpool::new(&template).unwrap();
    spool
        .push(&found(0..1, vec![Some(0..1), Some(0..1)]))
        .unwrap();
    spool.file.seek(SeekFrom::Start(16)).unwrap();
    spool.file.write_all(&[2]).unwrap();
    let mut records = spool.into_replacements().unwrap();
    assert!(matches!(
        records.next(),
        Some(Err(
            MarkdownDocumentBufferError::ReplacementInputUnavailable
        ))
    ));
    assert!(records.next().is_none());
}
