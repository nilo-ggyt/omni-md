//! Compare actual file-backed structural windows with fixed source coordinates and a finite independent complete-Parser oracle.

use super::*;
use crate::markdown_viewport_presentation::table_window::{
    MarkdownTableGlobalContext, MarkdownTableSourceLine, MarkdownTableWindowInput,
    project_table_window,
};
use crate::markdown_viewport_presentation::{
    MarkdownPresentationStyles, MarkdownViewportPresentation,
};

fn document(source: &str) -> (tempfile::TempDir, MarkdownDocumentBuffer) {
    // The returned TempDir owns this synthetic file; the buffer uses the same public open path as a normal note.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("S2-structure-fixture.md");
    std::fs::write(&path, source).unwrap();
    let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap();
    (directory, document)
}

fn finish(
    request: &mut MarkdownTableWindowStructureRequest,
    doc: &MarkdownDocumentBuffer,
) -> MarkdownTableStructureStep {
    // Only the test driver loops synchronously; the product scheduler advances once and resumes outside the current event stack.
    for _ in 0..doc.byte_len().saturating_mul(32).saturating_add(10000) {
        let step = request.advance(doc, false, 4096).unwrap();
        let work = match step {
            MarkdownTableStructureStep::Progress { work }
            | MarkdownTableStructureStep::StructureReady { work }
            | MarkdownTableStructureStep::TableEndReached { work }
            | MarkdownTableStructureStep::NotTable { work } => work,
        };
        assert!(work.byte_reads <= 4096 && work.transitions <= 4096);
        if !matches!(step, MarkdownTableStructureStep::Progress { .. }) {
            return step;
        }
    }
    panic!("finite fixture did not complete within actual work-derived test allowance");
}

#[test]
// Request a few distant rows and retain absolute ordinals, while exported metadata adds no strong document-root owner.
fn real_large_table_request_keeps_only_absolute_requested_rows_and_no_source_owner() {
    let mut source = String::from("| H | C |\n| - | - |\n");
    for row in 1..=5000 {
        source.push_str(&format!("| ROW{row:04} | 中文😀 |\n"));
    }
    let start_byte = source.find("| ROW4500").unwrap();
    let end_byte = source.find("| ROW4505").unwrap();
    let start = source[..start_byte].encode_utf16().count();
    let end = source[..end_byte].encode_utf16().count();
    let (_directory, doc) = document(&source);
    let owners = Arc::strong_count(doc.root.as_ref().unwrap());
    let mut request = doc
        .table_window_structure_request(MarkdownTableWindowTarget::AtUtf16(start), start..end)
        .unwrap();
    assert!(request.structural_window_for(&doc).unwrap().is_none());
    assert!(matches!(
        finish(&mut request, &doc),
        MarkdownTableStructureStep::StructureReady { .. }
    ));
    let window = request.structural_window_for(&doc).unwrap().unwrap();
    assert_eq!(
        window
            .rows()
            .iter()
            .map(|row| row.row_ordinal)
            .collect::<Vec<_>>(),
        vec![4500, 4501, 4502, 4503, 4504]
    );
    assert!(!window.includes_header());
    assert!(window.table_end().is_none());
    assert_eq!(Arc::strong_count(doc.root.as_ref().unwrap()), owners);
    assert!(request.total_work().byte_reads >= end_byte);
    assert!(window.rows().len() <= TABLE_WINDOW_MAX_ROWS);
}

#[test]
// Grammar inside an omitted row must not change the projected later row when compared with the unchanged complete source.
fn literal_dollars_and_braces_keep_omitted_row_projection_equal_to_full_source() {
    let hidden = format!("$x${}", "{}".repeat(255));
    let source = format!("| H |\n| - |\n| {hidden} |\n| $}}x{{y$ |\n");
    let start = source.rfind("| $}x{y$ |").unwrap();
    let (_directory, doc) = document(&source);
    let mut request = doc
        .table_window_structure_request(
            MarkdownTableWindowTarget::AtUtf16(start),
            start..source.len(),
        )
        .unwrap();
    finish(&mut request, &doc);
    let ticket = request.structural_window_for(&doc).unwrap().unwrap();
    let header = ticket.header;
    let row = ticket.body[0];
    // Copy only this finite fixture's proved header, delimiter, and target row for the public projection boundary.
    let h = doc
        .copy_table_source_range(header.header_byte_start..header.header_byte_end, 65536)
        .unwrap();
    let d = doc
        .copy_table_source_range(
            header.separator_byte_start..header.separator_byte_end,
            65536,
        )
        .unwrap();
    let body = doc
        .copy_table_source_range(row.byte_start..row.byte_end, 65536)
        .unwrap();
    let rows = [MarkdownTableSourceLine {
        text: &body,
        byte_start: row.byte_start,
        utf16_start: row.utf16_start,
        source_line: row.source_line,
        row_ordinal: row.row_ordinal,
        container_prefix_bytes: row.container_prefix_bytes,
    }];
    let window = project_table_window(&MarkdownTableWindowInput {
        generation: doc.generation(),
        context_revision: doc.generation(),
        header: MarkdownTableSourceLine {
            text: &h,
            byte_start: header.header_byte_start,
            utf16_start: ticket.header_utf16.start,
            source_line: header.header_source_line,
            row_ordinal: 0,
            container_prefix_bytes: header.header_prefix_bytes,
        },
        delimiter: MarkdownTableSourceLine {
            text: &d,
            byte_start: header.separator_byte_start,
            utf16_start: ticket.delimiter_utf16.start,
            source_line: header.separator_source_line,
            row_ordinal: 0,
            container_prefix_bytes: header.separator_prefix_bytes,
        },
        body: &rows,
        include_header: false,
        inherited_styles: MarkdownPresentationStyles::default(),
        container_prefix_runs: &[],
        global: MarkdownTableGlobalContext::Ready {
            reference_definitions: "",
            underline: &[],
        },
        active_selection: usize::MAX..usize::MAX,
    })
    .unwrap();
    let mut oracle = MarkdownViewportPresentation::default();
    // Whole-source parsing is confined to this finite test oracle, independently of the window-under-test's metadata.
    oracle.set_active_selection(usize::MAX..usize::MAX);
    oracle
        .rebuild(doc.generation(), 0..source.encode_utf16().count(), &source)
        .unwrap();
    for at in row.utf16_start..row.utf16_end {
        let style = |runs: &[crate::markdown_viewport_presentation::MarkdownPresentationRun]| {
            runs.iter()
                .find(|run| run.utf16_range.contains(&at))
                .map_or(MarkdownPresentationStyles::default(), |run| run.styles)
        };
        assert_eq!(style(&window.runs), style(oracle.runs()));
    }
    assert_eq!(ticket.table_end().unwrap().last_body_row_ordinal(), Some(2));
}

#[test]
// Exhausting one cell batch cannot masquerade as table End; a separate real required-row replay establishes actual EOF membership.
fn membership_end_and_window_batch_boundary_are_separate_permissions() {
    let h = format!("|{}\n", " h |".repeat(256));
    let d = format!("|{}\n", " - |".repeat(256));
    let row = format!("|{}\n", " v |".repeat(256));
    let source = format!("{h}{d}{}", row.repeat(20));
    let (_directory, doc) = document(&source);
    let start = h.len() + d.len();
    let mut request = doc
        .table_window_structure_request(
            MarkdownTableWindowTarget::AtUtf16(start),
            start..source.len(),
        )
        .unwrap();
    finish(&mut request, &doc);
    let window = request.structural_window_for(&doc).unwrap().unwrap();
    assert_eq!(window.rows().len(), 3); // Header plus three 256-column rows consumes this batch's 1024 cells, without disabling later rows.
    assert_eq!(window.next_required_row(), Some(4));
    assert!(window.table_end().is_none());
    let mut after = doc
        .table_window_structure_request(
            MarkdownTableWindowTarget::RequiredRow {
                header_byte_start: 0,
                row_ordinal: 21,
            },
            start..start,
        )
        .unwrap();
    assert!(matches!(
        finish(&mut after, &doc),
        MarkdownTableStructureStep::TableEndReached { .. }
    ));
    let end = after.required_row_end_for(&doc).unwrap().unwrap();
    assert_eq!(end.last_body_row_ordinal(), Some(20));
    assert!(end.ended_at_source_eof());
    assert!(after.structural_window_for(&doc).unwrap().is_none());
}

#[test]
// Scan through a giant opaque prefix incrementally, then compare the actual quoted table's UTF-16 coordinates with original bytes.
fn giant_opaque_prefix_and_utf16_coordinates_remain_bounded_and_true() {
    let source = format!(
        "```\n{}\n```\n\n> | 标题😀 |\r\n> | - |\r\n> | 内容😀 |\r\n",
        "中".repeat(350000)
    );
    let target_byte = source.find("> | 内容").unwrap();
    let target = source[..target_byte].encode_utf16().count();
    let (_directory, doc) = document(&source);
    let mut request = doc
        .table_window_structure_request(
            MarkdownTableWindowTarget::AtUtf16(target),
            target..doc.utf16_len(),
        )
        .unwrap();
    finish(&mut request, &doc);
    let ticket = request.structural_window_for(&doc).unwrap().unwrap();
    assert_eq!(ticket.rows().len(), 1);
    assert_eq!(ticket.rows()[0].container_prefix_bytes, 2);
    assert_eq!(
        ticket.header_utf16.start,
        source[..ticket.header.header_byte_start]
            .encode_utf16()
            .count()
    );
    assert_eq!(
        ticket.header_utf16.end,
        source[..ticket.header.header_byte_end]
            .encode_utf16()
            .count()
    );
    assert_eq!(
        ticket.delimiter_utf16.start,
        source[..ticket.header.separator_byte_start]
            .encode_utf16()
            .count()
    );
    assert_eq!(
        ticket.delimiter_utf16.end,
        source[..ticket.header.separator_byte_end]
            .encode_utf16()
            .count()
    );
    assert!(request.total_work().byte_reads >= 1024 * 1024);
}

#[test]
// Equal text, restored roots, and cleared cancellation flags cannot renew a request after its original authority fails.
fn stale_source_ready_cancel_and_undo_cannot_revive_structure_authority() {
    let source = "| H |\n| - |\n| body |\n";
    let (_directory, mut doc) = document(source);
    let (_other_directory, other) = document(source);
    let mut request = doc
        .table_window_structure_request(MarkdownTableWindowTarget::AtUtf16(12), 12..doc.utf16_len())
        .unwrap();
    finish(&mut request, &doc);
    assert_eq!(
        request.advance(&other, false, 4096),
        Err(MarkdownTableRequestError::SourceSuperseded)
    );
    // Failure is sticky: returning to the original note cannot revive this retired request.
    assert_eq!(
        request.advance(&doc, false, 4096),
        Err(MarkdownTableRequestError::SourceSuperseded)
    );
    let mut before_edit = doc
        .table_window_structure_request(MarkdownTableWindowTarget::AtUtf16(12), 12..doc.utf16_len())
        .unwrap();
    doc.replace_utf16_range(14..14, "new", MarkdownTextSelection::caret(17))
        .unwrap();
    doc.undo();
    assert_eq!(
        before_edit.advance(&doc, false, 4096),
        Err(MarkdownTableRequestError::SourceSuperseded)
    );
    let mut cancelled = doc
        .table_window_structure_request(MarkdownTableWindowTarget::AtUtf16(12), 12..doc.utf16_len())
        .unwrap();
    assert_eq!(
        cancelled.advance(&doc, true, 4096),
        Err(MarkdownTableRequestError::Cancelled)
    );
    assert_eq!(
        cancelled.advance(&doc, false, 4096),
        Err(MarkdownTableRequestError::Cancelled)
    );
}

#[test]
// A probe within a table must still recover earlier visible rows and header from that same actual table, without widening requested authority.
fn probe_in_middle_covers_earlier_visible_rows_and_header_from_the_same_table() {
    let source = "| H |\n| - |\n| first |\n| second |\n| third |\n| fourth |\n";
    let (_directory, doc) = document(source);
    let at = source.find("| second").unwrap() + 2;
    let end = source.find("| fourth").unwrap();
    let mut request = doc
        .table_window_structure_request(MarkdownTableWindowTarget::AtUtf16(at), 0..end)
        .unwrap();
    finish(&mut request, &doc);
    let ticket = request.structural_window_for(&doc).unwrap().unwrap();
    assert!(ticket.includes_header());
    assert_eq!(
        ticket
            .rows()
            .iter()
            .map(|row| row.row_ordinal)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    // The probe must lie within the requested viewport; an unrelated display range cannot receive this structural claim.
    assert!(
        doc.table_window_structure_request(MarkdownTableWindowTarget::AtUtf16(at), 0..12)
            .is_err()
    );
}

#[test]
// A full front batch is not proof of membership at a later target; retain actual batch continuation and withhold table End.
fn row_limit_includes_context_header_and_full_front_batch_waits_for_real_target_membership() {
    let mut source = String::from("| H |\n| - |\n");
    for row in 1..=300 {
        source.push_str(&format!("| R{row:03} |\n"));
    }
    let at = source.find("| R290").unwrap() + 2;
    let (_directory, doc) = document(&source);
    let mut request = doc
        .table_window_structure_request(MarkdownTableWindowTarget::AtUtf16(at), 0..source.len())
        .unwrap();
    finish(&mut request, &doc);
    let ticket = request.structural_window_for(&doc).unwrap().unwrap();
    assert!(ticket.includes_header());
    assert_eq!(ticket.rows().len(), 255); // The header and 255 body rows reach the same 256-row projection limit.
    assert_eq!(ticket.rows().first().unwrap().row_ordinal, 1);
    assert_eq!(ticket.rows().last().unwrap().row_ordinal, 255);
    assert_eq!(ticket.next_required_row(), Some(256));
    assert!(ticket.table_end().is_none());
}
