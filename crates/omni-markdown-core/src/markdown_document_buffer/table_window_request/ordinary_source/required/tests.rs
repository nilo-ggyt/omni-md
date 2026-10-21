//! Exercise same-request structural navigation through real rows, End, revocation, and mapping release rather than private cursor counts.

use super::*;

fn document(text: &str) -> (tempfile::NamedTempFile, MarkdownDocumentBuffer) {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), text).unwrap();
    let document = MarkdownDocumentBuffer::open(file.path(), 32 * 1024 * 1024).unwrap();
    (file, document)
}

fn authority(document: &MarkdownDocumentBuffer) -> MarkdownViewportSourceAuthority {
    MarkdownViewportSourceAuthority {
        context_revision: 71,
        selection: document.selection(),
        cancelled: false,
    }
}

/// Drive original production steps; source readiness grants neither ordinary styles nor a completed semantic table window.
fn finish(
    request: &mut MarkdownViewportSourceRequest,
    document: &MarkdownDocumentBuffer,
) -> VerifiedMarkdownViewportSources {
    loop {
        let work = match request
            .advance(document, authority(document), 4096)
            .unwrap()
        {
            MarkdownViewportSourceStep::Progress { work }
            | MarkdownViewportSourceStep::SourcesAvailable { work } => work,
        };
        assert!(work.source_bytes <= 4096);
        assert!(work.copied_source_bytes <= 4096);
        assert!(work.structural_transitions <= 4096);
        if let Some(sources) = request
            .take_sources_for(document, authority(document))
            .unwrap()
        {
            assert!(
                sources
                    .table_sources_for(document, authority(document))
                    .unwrap()
                    .windows_for(document)
                    .unwrap()
                    .next()
                    .is_none()
            );
            return sources;
        }
    }
}

fn start(
    document: &MarkdownDocumentBuffer,
    position: usize,
) -> (
    MarkdownViewportSourceRequest,
    VerifiedMarkdownViewportSources,
) {
    let mut request = document
        .viewport_leaf_source_request(position..position + 1, 71, document.selection())
        .unwrap();
    let sources = finish(&mut request, document);
    (request, sources)
}

/// Request navigation with an original row ticket and direction, preserving offscreen targets even when visual coverage stays fixed.
fn navigate(
    request: &mut MarkdownViewportSourceRequest,
    document: &MarkdownDocumentBuffer,
    sources: VerifiedMarkdownViewportSources,
    origin: &VerifiedMarkdownTableRowSource,
    direction: MarkdownTableWindowNavigation,
) -> (
    VerifiedMarkdownViewportSources,
    MarkdownViewportSourceRequiredRowResult,
) {
    let query = sources
        .required_row_query_for(document, authority(document), origin, direction)
        .unwrap()
        .unwrap();
    let viewport = sources
        .requested_for(document, authority(document))
        .unwrap()
        .clone();
    request
        .continue_forward_with_required_row(
            document,
            &sources,
            viewport,
            authority(document),
            query,
        )
        .unwrap();
    let next = finish(request, document);
    let result = next
        .required_row_result_for(document, authority(document))
        .unwrap()
        .unwrap();
    (next, result)
}

#[test]
// Keep visual coverage fixed while actual neighboring-row queries walk to the header and then the real table terminal.
fn structural_navigation_walks_backward_to_header_forward_to_true_end_and_releases_mapping() {
    let text = "前段\r\n\r\n| H | C |\r\n| - | - |\r\n| 第一行中😀 | 一 |\r\n| second | 二 |\r\n| third | 三 |\r\n\r\n后继正文\r\n";
    let (file, document) = document(text);
    let mapping = document.original_backing_observer().unwrap();
    let position = text[..text.find("| third").unwrap()].encode_utf16().count();
    let (mut request, mut sources) = start(&document, position);
    let mut row = sources
        .table_row_at_for(&document, authority(&document), position)
        .unwrap()
        .unwrap();
    assert_eq!(
        row.coordinate_for(&document, authority(&document))
            .unwrap()
            .1,
        3
    );
    for expected in [2, 1, 0] {
        let (next, result) = navigate(
            &mut request,
            &document,
            sources,
            &row,
            MarkdownTableWindowNavigation::Previous,
        );
        sources = next;
        let MarkdownViewportSourceRequiredRowResult::Row { row: actual } = result else {
            panic!("原前一成员不能由 End 代替")
        };
        row = actual;
        assert_eq!(
            row.coordinate_for(&document, authority(&document))
                .unwrap()
                .1,
            expected
        );
        let range = row
            .physical_utf16_range_for(&document, authority(&document))
            .unwrap();
        let needle = match expected {
            0 => "| H",
            1 => "| 第一行",
            _ => "| second",
        };
        let start = text[..text.find(needle).unwrap()].encode_utf16().count();
        assert_eq!(range.start, start);
    }
    assert!(
        sources
            .required_row_query_for(
                &document,
                authority(&document),
                &row,
                MarkdownTableWindowNavigation::Previous
            )
            .unwrap()
            .is_none()
    );
    for expected in [1, 2, 3] {
        let (next, result) = navigate(
            &mut request,
            &document,
            sources,
            &row,
            MarkdownTableWindowNavigation::NextRow,
        );
        sources = next;
        let MarkdownViewportSourceRequiredRowResult::Row { row: actual } = result else {
            panic!("真实后继行不能被 End 代替")
        };
        row = actual;
        assert_eq!(
            row.coordinate_for(&document, authority(&document))
                .unwrap()
                .1,
            expected
        );
    }
    let (sources, result) = navigate(
        &mut request,
        &document,
        sources,
        &row,
        MarkdownTableWindowNavigation::Next,
    );
    let MarkdownViewportSourceRequiredRowResult::TableEnd { terminal } = result else {
        panic!("后继正文不是表格行，必须取得真正 End")
    };
    assert_eq!(
        terminal
            .coordinate_for(&document, authority(&document))
            .unwrap()
            .1,
        3
    );
    let range = terminal
        .physical_utf16_range_for(&document, authority(&document))
        .unwrap();
    assert_eq!(range.start, position);
    assert_eq!(std::fs::read(file.path()).unwrap(), text.as_bytes());
    request.cancel();
    drop((request, sources, row, document));
    // An exported terminal retains weak identity/checkpoint metadata only; it cannot keep the closed document's mapping alive.
    assert!(mapping.unmap_succeeded());
    assert!(!mapping.failed());
    drop(terminal);
}

#[test]
// Foreign query rejection is recoverable preflight; actual document generation changes revoke previously issued row authority.
fn structural_navigation_rejects_foreign_publication_document_and_undo_stale_origins() {
    let text = "| H | C |\n| - | - |\n| a | b |\n| c | d |\n";
    let (_file, mut document) = document(text);
    let position = text.find("| a").unwrap();
    let (mut request, sources) = start(&document, position);
    let row = sources
        .table_row_at_for(&document, authority(&document), position)
        .unwrap()
        .unwrap();
    // Neighbor navigation cannot extend visual coverage into the unknown left side; the original checkpoint resolves that row offscreen.
    let query = sources
        .required_row_query_for(
            &document,
            authority(&document),
            &row,
            MarkdownTableWindowNavigation::Previous,
        )
        .unwrap()
        .unwrap();
    assert!(matches!(
        request.continue_forward_with_required_row(
            &document,
            &sources,
            0..position + 1,
            authority(&document),
            query
        ),
        Err(MarkdownTableRequestError::CoverageRequiresCheckpoint)
    ));
    let (_foreign_request, foreign_sources) = start(&document, position);
    let foreign_row = foreign_sources
        .table_row_at_for(&document, authority(&document), position)
        .unwrap()
        .unwrap();
    assert!(matches!(
        sources.required_row_query_for(
            &document,
            authority(&document),
            &foreign_row,
            MarkdownTableWindowNavigation::Next
        ),
        Err(MarkdownTableRequestError::PublicationSuperseded)
    ));
    let query = foreign_sources
        .required_row_query_for(
            &document,
            authority(&document),
            &foreign_row,
            MarkdownTableWindowNavigation::Next,
        )
        .unwrap()
        .unwrap();
    assert!(matches!(
        request.continue_forward_with_required_row(
            &document,
            &sources,
            position..position + 1,
            authority(&document),
            query
        ),
        Err(MarkdownTableRequestError::PublicationSuperseded)
    ));
    let (_other_file, other) = self::document(text);
    assert!(matches!(
        sources.required_row_query_for(
            &other,
            authority(&other),
            &row,
            MarkdownTableWindowNavigation::Next
        ),
        Err(MarkdownTableRequestError::SourceSuperseded)
    ));
    // Foreign-query preflight rejection preserves the owner, which must still resolve its legitimate next-row query.
    let (_next, result) = navigate(
        &mut request,
        &document,
        sources,
        &row,
        MarkdownTableWindowNavigation::Next,
    );
    assert!(matches!(
        result,
        MarkdownViewportSourceRequiredRowResult::Row { .. }
    ));
    document
        .insert_utf16(0, "新", MarkdownTextSelection::caret(1))
        .unwrap();
    document.undo_text_edit().unwrap();
    assert!(matches!(
        row.coordinate_for(&document, authority(&document)),
        Err(MarkdownTableRequestError::SourceSuperseded)
    ));
    assert_eq!(std::fs::read(document.path()).unwrap(), text.as_bytes());
}

#[test]
// Adding terminal evidence must preserve all previously visible members, so backward navigation still resolves their original coordinates.
fn structural_navigation_end_preserves_original_multirow_physical_coverage() {
    let text = "| H | C |\r\n| - | - |\r\n| first | 一 |\r\n| second | 二 |\r\n| third | 三 |\r\n\r\n后继正文\r\n";
    let (_file, document) = document(text);
    let last = text[..text.find("| third").unwrap()].encode_utf16().count();
    // Close coverage within the last row: the ticket already includes the header and three rows, but End still needs the next physical line.
    let mut request = document
        .viewport_leaf_source_request(0..last + 1, 71, document.selection())
        .unwrap();
    let sources = finish(&mut request, &document);
    let row = sources
        .table_row_at_for(&document, authority(&document), last)
        .unwrap()
        .unwrap();
    let (sources, result) = navigate(
        &mut request,
        &document,
        sources,
        &row,
        MarkdownTableWindowNavigation::Next,
    );
    let MarkdownViewportSourceRequiredRowResult::TableEnd { terminal } = result else {
        panic!("末行后是真正表尾")
    };
    assert_eq!(
        terminal
            .coordinate_for(&document, authority(&document))
            .unwrap()
            .1,
        3
    );
    for (ordinal, needle) in [(0, "| H"), (1, "| first"), (2, "| second"), (3, "| third")] {
        let position = text[..text.find(needle).unwrap()].encode_utf16().count();
        let preserved = sources
            .table_row_at_for(&document, authority(&document), position)
            .unwrap()
            .unwrap();
        assert_eq!(
            preserved
                .coordinate_for(&document, authority(&document))
                .unwrap()
                .1,
            ordinal
        );
    }
    // Attaching real End preserves the visible multirow ticket; subsequent backward navigation still consumes this same owner.
    let (_sources, result) = navigate(
        &mut request,
        &document,
        sources,
        &terminal,
        MarkdownTableWindowNavigation::Previous,
    );
    let MarkdownViewportSourceRequiredRowResult::Row { row } = result else {
        panic!("前一行仍是真实成员")
    };
    assert_eq!(
        row.coordinate_for(&document, authority(&document))
            .unwrap()
            .1,
        2
    );
}
