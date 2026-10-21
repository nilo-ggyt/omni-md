//! Exercise actual bounded table semantics against the original finite full-parser oracle.

use super::*;
use crate::markdown_viewport_presentation::{
    MarkdownPresentationStyles, MarkdownViewportPresentation,
};

fn document(source: &str) -> (tempfile::TempDir, MarkdownDocumentBuffer) {
    // Own an isolated actual source file; no user note or product workspace is
    // accessed by the original-parser comparison.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("S2-semantic-fixture.md");
    std::fs::write(&path, source).unwrap();
    let doc = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap();
    (directory, doc)
}
fn finish(
    request: &mut MarkdownTableWindowRequest,
    document: &MarkdownDocumentBuffer,
) -> MarkdownTableWindowStep {
    // Yielded work remains pending. Drive the same original request under its
    // real 4096-unit quantum until an actual terminal response is delivered.
    for _ in 0..document.byte_len().saturating_mul(64).saturating_add(10000) {
        let step = request.advance(document, false, 4096).unwrap();
        if !matches!(step, MarkdownTableWindowStep::Progress { .. }) {
            return step;
        }
    }
    panic!("semantic fixture exceeded finite driver allowance");
}
fn assert_matches_original(
    source: &str,
    probe: usize,
    viewport: Range<usize>,
    selection: Range<usize>,
) {
    // Preserve the original directed selection/context and source while preparing
    // a real window; the finite independent parser is only a test output oracle.
    let (_directory, doc) = document(source);
    let owners = Arc::strong_count(doc.root.as_ref().unwrap());
    let mut request = doc
        .table_window_request(
            MarkdownTableWindowTarget::AtUtf16(probe),
            viewport,
            93,
            selection.clone(),
        )
        .unwrap();
    assert!(matches!(
        finish(&mut request, &doc),
        MarkdownTableWindowStep::Ready { .. }
    ));
    assert!(request.lines.is_empty() && request.lines.capacity() == 0);
    assert!(
        request.queries.is_none()
            && request.pending_reference.is_none()
            && request.references.is_none()
            && request.reference_fact.is_none()
    );
    assert!(request.copier.is_none() && request.scope_cursor.is_none() && request.ticket.is_none());
    assert!(request.prefix_runs.is_empty() && request.prefix_runs.capacity() == 0);
    assert!(request.underline.is_none());
    assert_eq!(Arc::strong_count(doc.root.as_ref().unwrap()), owners);
    let window = request.ready_for(&doc).unwrap().unwrap();
    let presentation = window.presentation_for(&doc).unwrap();
    assert_eq!(presentation.context_revision, 93);
    let mut original = MarkdownViewportPresentation::default();
    // Compare actual physical coverage one UTF-16 unit at a time. A retained
    // header context cannot grant styles to an omitted interval outside that coverage.
    original.set_active_selection(selection);
    original
        .rebuild(doc.generation(), 0..doc.utf16_len(), source)
        .unwrap();
    for coverage in window.coverage_for(&doc).unwrap() {
        for at in coverage.clone() {
            let style = |runs: &[MarkdownPresentationRun]| {
                runs.iter()
                    .find(|run| run.utf16_range.contains(&at))
                    .map_or(MarkdownPresentationStyles::default(), |run| run.styles)
            };
            assert_eq!(
                style(&presentation.runs),
                style(original.runs()),
                "at={at} source={source:?}"
            );
        }
    }
}

#[test]
fn complete_request_preserves_full_physical_container_prefix_and_unicode_styles() {
    // Compare whole physical coverage, including actual header/delimiter/body
    // container prefixes and original EOL units, not cell interiors alone.
    for source in [
        "| H | C |\n| - | - |\n| **一** | $x$ |\n| [r] | ~~删~~ |\n\n[r]: /first \"title\"\n[r]: /later\n",
        "> > | H | C |\r\n> > | - | - |\r\n> > | **一** | $x$ |\r\n> > | [r] | ~~删~~ |\r\n\r\n[r]: /r\r\n",
        "-   item\n\n    > | H | C |\n    > | - | - |\n    > | **一** | $x$ |\n    > | [r] | ~~删~~ |\n\n[r]: /r\n",
        "[^n]:\n    | H | C |\n    | - | - |\n    | **一** | $x$ |\n    | [r] | ~~删~~ |\n\n[r]: /r\n\n[^n]\n",
        "3. | H | C |\n   | - | - |\n   | **一** | $x$ |\n   | [r] | ~~删~~ |\n\n[r]: /r\n",
    ] {
        let probe_byte = source.find("**一**").unwrap();
        let probe = source[..probe_byte].encode_utf16().count();
        assert_matches_original(
            source,
            probe,
            0..source.encode_utf16().count(),
            usize::MAX..usize::MAX,
        );
        // Omitting the header preserves actual body-container attributes without
        // granting source/style coverage to a virtually joined intervening gap.
        let body_start = source[..probe_byte].rfind('\n').map_or(0, |at| at + 1);
        let start = source[..body_start].encode_utf16().count();
        assert_matches_original(
            source,
            probe,
            start..source.encode_utf16().count(),
            probe..probe,
        );
    }
}

#[test]
fn global_reference_and_underline_remain_required_around_literal_dollars() {
    // Literal dollar text cannot waive real source-order reference and underline
    // context extending before/after this small requested physical row.
    let hidden = format!("$x${}", "{}".repeat(255));
    let source = format!(
        "<u>opening\n\n| H | C |\n| - | - |\n| {hidden} | skipped |\n| $}}x{{y$ [r] | 中文😀 |\n\n</u>\n\n[r]: /real \"title\"\n"
    );
    let probe_byte = source.find("$}x{y$").unwrap();
    let probe = source[..probe_byte].encode_utf16().count();
    let start_byte = source[..probe_byte].rfind('\n').unwrap() + 1;
    let end_byte = source[probe_byte..].find('\n').unwrap() + probe_byte + 1;
    let viewport =
        source[..start_byte].encode_utf16().count()..source[..end_byte].encode_utf16().count();
    assert_matches_original(&source, probe, viewport, usize::MAX..usize::MAX);
}

#[test]
fn standalone_table_underline_uses_literal_code_facts_before_and_after_its_viewport() {
    for source in [
        "<u>外部开标记\n\n```text\n</u>围栏字面\n```\n\n| H | C |\n| - | - |\n| [r] | 中文😀 |\n\n    </u><u>缩进字面\n\n外部真实关闭</u>\n\n[r]: /first\n[r]: /later\n",
        "<u>外部开标记\r\n\r\n>\t```text\r\n>\t</u>围栏字面\r\n>\t```\r\n>\r\n> | H | C |\r\n> | - | - |\r\n> | [r] | 中文😀 |\r\n\r\n    </u><u>缩进字面\r\n\r\n外部真实关闭</u>\r\n\r\n[r]: /first\r\n[r]: /later\r\n",
    ] {
        let probe_byte = source.find("[r]").unwrap();
        let probe = source[..probe_byte].encode_utf16().count();
        let start = source[..probe_byte].rfind('\n').unwrap() + 1;
        let end = probe_byte + source[probe_byte..].find('\n').unwrap() + 1;
        // Keep the standalone original structural Global alive until pairing
        // restores it; another classifier is not an equivalent continuation.
        assert_matches_original(
            source,
            probe,
            source[..start].encode_utf16().count()..source[..end].encode_utf16().count(),
            usize::MAX..usize::MAX,
        );
    }
}

#[test]
fn verified_window_cannot_authorize_other_note_or_stale_edit_and_releases_scratch() {
    // A real published window authorizes only its original note/root/generation.
    // One actual cell edit, even after undo, revokes the old window and request.
    let source = "| H | C |\n| - | - |\n| first | second |\n";
    let (_directory, mut doc) = document(source);
    let (_other_directory, other) = document(source);
    let probe = source.find("first").unwrap();
    let mut request = doc
        .table_window_request(
            MarkdownTableWindowTarget::AtUtf16(probe),
            0..doc.utf16_len(),
            4,
            usize::MAX..usize::MAX,
        )
        .unwrap();
    finish(&mut request, &doc);
    let window = request.take_ready_for(&doc).unwrap().unwrap();
    assert!(request.ready_for(&doc).unwrap().is_none());
    assert_eq!(
        window.presentation_for(&other).unwrap_err(),
        MarkdownTableRequestError::SourceSuperseded
    );
    let plan = doc
        .plan_verified_table_cell_edit(&window, 1, 0, "updated")
        .unwrap();
    let (range, replacement) = plan.replacement_for(&doc).unwrap();
    doc.replace_utf16_range(range.clone(), replacement, MarkdownTextSelection::caret(0))
        .unwrap();
    assert!(window.presentation_for(&doc).is_err());
    doc.undo();
    assert!(window.presentation_for(&doc).is_err());
    assert_eq!(
        request.advance(&doc, false, 4096),
        Err(MarkdownTableRequestError::SourceSuperseded)
    );
    assert!(request.lines.is_empty() && request.lines.capacity() == 0);
}

#[test]
fn consumed_request_cannot_republish_an_empty_ready_slot() {
    // Even in one generation, transfer Ready once. A consumed empty request
    // cannot signal that Native has another completed window to publish.
    let (_directory, doc) = document("| H |\n| - |\n| value |\n");
    let mut request = doc
        .table_window_request(
            MarkdownTableWindowTarget::AtUtf16(2),
            0..doc.utf16_len(),
            0,
            0..0,
        )
        .unwrap();
    finish(&mut request, &doc);
    assert!(request.take_ready_for(&doc).unwrap().is_some());
    assert_eq!(
        request.advance(&doc, false, 4096),
        Err(MarkdownTableRequestError::InvalidRequest)
    );
    assert!(request.take_ready_for(&doc).is_err());
}

#[test]
fn offscreen_header_retains_cell_styles_without_becoming_a_visible_row_or_gap() {
    // A middle window needs header cells for column measurement, but the omitted
    // interval between header and body has no style/geometry coverage authority.
    let source = "<u>before\n\n> | **中文** [r] | $x$ 😀 |\n> | - | - |\n> | skipped | old |\n> | visible | cell |\n\n</u>\n\n[r]: /real\n";
    let (_directory, doc) = document(source);
    let start = source.find("> | visible").unwrap();
    let end = start + source[start..].find('\n').unwrap() + 1;
    let units = source[..start].encode_utf16().count()..source[..end].encode_utf16().count();
    let mut request = doc
        .table_window_request(
            MarkdownTableWindowTarget::AtUtf16(units.start),
            units.clone(),
            7,
            usize::MAX..usize::MAX,
        )
        .unwrap();
    finish(&mut request, &doc);
    let ready = request.ready_for(&doc).unwrap().unwrap();
    let actual = ready.presentation_for(&doc).unwrap();
    assert_eq!(actual.rows.len(), 1);
    assert_eq!(actual.rows[0].row_ordinal, 2);
    assert_eq!(ready.coverage_for(&doc).unwrap(), &[units]);
    assert_eq!(actual.header_cells().len(), 2);
    assert!(actual.rows.iter().all(|row| !row.is_header));
    let mut original = MarkdownViewportPresentation::default();
    original.set_active_selection(usize::MAX..usize::MAX);
    original
        .rebuild(doc.generation(), 0..doc.utf16_len(), source)
        .unwrap();
    // Compare original header-cell ranges unit by unit; offscreen header-container
    // markers need not enter this window's actual drawing matrix.
    for cell in actual.header_cells() {
        if let crate::markdown_viewport_presentation::table_window::MarkdownTableWindowCellSource::Present(range) = &cell.source {
            for at in range.clone() {
                let style = |runs: &[MarkdownPresentationRun]| runs.iter().find(|run| run.utf16_range.contains(&at)).map_or(MarkdownPresentationStyles::default(), |run| run.styles);
                assert_eq!(style(&actual.header_context.runs), style(original.runs()), "header unit={at}");
            }
        }
    }
    assert!(
        actual
            .runs
            .iter()
            .all(|run| run.utf16_range.start >= actual.rows[0].utf16_range.start)
    );
}

#[test]
fn measured_validation_includes_revisited_prefix_and_scope_metadata_payload() {
    // Split a genuine Unicode scalar at a frame boundary to expose repeated
    // UTF-8 validation input; distinct source bytes alone undercount this work.
    let source = format!("| {}中 |\n| - |\n| value |\n", "a".repeat(996));
    let (_directory, doc) = document(&source);
    let mut request = doc
        .table_window_request(
            MarkdownTableWindowTarget::AtUtf16(0),
            0..doc.utf16_len(),
            0,
            0..0,
        )
        .unwrap();
    let mut reads = 0;
    let mut validation = 0;
    loop {
        let copying = matches!(request.phase, Phase::Copy);
        let step = request.advance(&doc, false, 4096).unwrap();
        if copying && let MarkdownTableWindowStep::Progress { work } = step {
            reads += work.source_bytes;
            validation += work.utf8_validation_bytes;
            // Sum only the actual byte-copy phase. Whole-slice Parser/marker
            // inputs are reported separately and do not prove per-token fuel bounds.
            assert!(
                work.source_bytes
                    + work.copied_source_bytes
                    + work.utf8_validation_bytes
                    + work.source_tree_steps
                    + work.structural_transitions
                    <= 4096
            );
        }
        if !matches!(step, MarkdownTableWindowStep::Progress { .. }) {
            break;
        }
    }
    assert!(
        validation > reads,
        "frame尾scalar使valid prefix实际被再次校验，必须如实计量"
    );

    // Actual quote scope requires visible-header marker metadata only when that
    // header is published; offscreen syntax context retains no fictional output payload.
    let prefix = "> ".repeat(16);
    let source = format!("{prefix}| H |\n{prefix}| - |\n{prefix}| value |\n");
    let (_directory, doc) = document(&source);
    let body_start = source.rfind(&format!("{prefix}| value")).unwrap();
    for (viewport, expected_output) in [
        (0..doc.utf16_len(), true),
        (body_start..doc.utf16_len(), false),
    ] {
        let mut request = doc
            .table_window_request(
                MarkdownTableWindowTarget::AtUtf16(viewport.start),
                viewport,
                0,
                0..0,
            )
            .unwrap();
        let mut metadata = 0;
        loop {
            let step = request.advance(&doc, false, 4096).unwrap();
            let work = match step {
                MarkdownTableWindowStep::Progress { work }
                | MarkdownTableWindowStep::Ready { work } => work,
                _ => panic!("quote fixture lost its table"),
            };
            metadata += work.metadata_bytes;
            if matches!(step, MarkdownTableWindowStep::Ready { .. }) {
                break;
            }
        }
        // Visible headers admit actual marker output. Offscreen headers keep
        // fixed kind facts rather than inventing Vec payload that was never produced.
        assert_eq!(metadata > 0, expected_output);
    }
}

#[test]
fn viewport_target_finds_real_tables_below_plain_text_and_continues_after_true_end() {
    // Actual source classification rejects a fenced fake table and locates two
    // later tables in order, preserving the first table's genuine end boundary.
    let source = "intro\n\n```text\n| fake |\n| - |\n```\n\n| first |\n| - |\n| body |\n\nordinary\n\n| second |\n| - |\n| tail |\n";
    let (_directory, doc) = document(source);
    let mut first = doc
        .table_window_request(
            MarkdownTableWindowTarget::FirstTableInViewport,
            0..doc.utf16_len(),
            0,
            0..0,
        )
        .unwrap();
    assert!(matches!(
        finish(&mut first, &doc),
        MarkdownTableWindowStep::Ready { .. }
    ));
    let window = first.take_ready_for(&doc).unwrap().unwrap();
    assert_eq!(
        window
            .presentation_for(&doc)
            .unwrap()
            .header_byte_range
            .start,
        source.find("| first").unwrap()
    );
    let after_first = window.table_end_for(&doc).unwrap().unwrap().utf16_end();
    let mut second = doc
        .table_window_request(
            MarkdownTableWindowTarget::FirstTableInViewport,
            after_first..doc.utf16_len(),
            0,
            0..0,
        )
        .unwrap();
    finish(&mut second, &doc);
    let window = second.take_ready_for(&doc).unwrap().unwrap();
    assert_eq!(
        window
            .presentation_for(&doc)
            .unwrap()
            .header_byte_range
            .start,
        source.find("| second").unwrap()
    );
    // A no-table proof for this finite prefix does not deny later document
    // tables or authorize bringing them into the current projection.
    let mut prefix = doc
        .table_window_request(
            MarkdownTableWindowTarget::FirstTableInViewport,
            0..5,
            0,
            0..0,
        )
        .unwrap();
    assert!(matches!(
        finish(&mut prefix, &doc),
        MarkdownTableWindowStep::NotTable { .. }
    ));
}

#[test]
/// Quote layers use original header anchors/physical endpoints, never a literal '>' inside footnote labels.
fn quote_prefix_owners_match_full_parser_for_endpoint_lines_and_footnote_labels() {
    for source in [
        "> > | H | C |\r\n> > | - | - |\r\n> > | 中文😀 | x |\r\n> > | 尾部 | y |",
        "[^a>]: > | H | C |\n    > | - | - |\n    > | 中文😀 | x |\n    > | 尾部 | y |\n\n[^a>]\n",
        "> [^a>]: > | H | C |\n>     > | - | - |\n>     > | 中文😀 | x |\n>     > | 尾部 | y |\n\n[^a>]\n",
    ] {
        let body_byte = source.find("中文").unwrap();
        let probe = source[..body_byte].encode_utf16().count();
        let line_byte = source[..body_byte].rfind('\n').map_or(0, |at| at + 1);
        let line_start = source[..line_byte].encode_utf16().count();
        let length = source.encode_utf16().count();
        for selection in [
            usize::MAX..usize::MAX,
            line_start..line_start,
            length..probe,
        ] {
            // Compare hidden bits and global UTF-16 positions exactly. A middle
            // gap still grants no nonexistent header coverage.
            assert_matches_original(source, probe, 0..length, selection.clone());
            assert_matches_original(source, probe, line_start..length, selection);
        }
    }
}
