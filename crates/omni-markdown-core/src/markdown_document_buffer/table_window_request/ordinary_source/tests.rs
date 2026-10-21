//! Synthetic real files and a complete Parser provide independent test oracles; their body copies and event Vecs are not product requests.

use super::*;
use crate::MarkdownPresentationStyles;

/// Publish a giant cell's real structural row without entering the 256-KiB LineCopy path; retain metadata and the same Global only.
#[test]
fn first_giant_table_row_publishes_real_structure_without_copying_or_reparsing_the_row() {
    use std::io::Write;
    let mut file = tempfile::NamedTempFile::new().unwrap();
    let header = "|H|C|\r\n|---|---|\r\n";
    file.write_all(header.as_bytes()).unwrap();
    file.write_all(b"|").unwrap();
    let page = [b'a'; 4096];
    for _ in 0..80 {
        file.write_all(&page).unwrap();
    }
    file.write_all(b"|tail|\r\n").unwrap();
    file.flush().unwrap();
    let document = MarkdownDocumentBuffer::open(file.path(), 8 * 1024 * 1024).unwrap();
    let authority = MarkdownViewportSourceAuthority {
        context_revision: 7,
        selection: document.selection(),
        cancelled: false,
    };
    let mut request = document
        .viewport_leaf_source_request(0..128, 7, authority.selection)
        .unwrap();
    let publication = loop {
        match request.advance(&document, authority, 4096).unwrap() {
            MarkdownViewportSourceStep::Progress { work } => {
                assert!(work.source_bytes <= 4096);
                assert!(work.structural_transitions <= 4096);
            }
            MarkdownViewportSourceStep::SourcesAvailable { .. } => {
                break request
                    .take_sources_for(&document, authority)
                    .unwrap()
                    .unwrap();
            }
        }
    };
    let h = publication
        .table_row_at_for(&document, authority, 0)
        .unwrap()
        .unwrap();
    assert_eq!(h.coordinate_for(&document, authority).unwrap(), (0, 0, 2));
    assert_eq!(
        h.physical_byte_range_for(&document, authority).unwrap(),
        0..header.len()
    );
    let row = publication
        .table_row_at_for(&document, authority, header.len())
        .unwrap()
        .unwrap();
    assert_eq!(row.coordinate_for(&document, authority).unwrap(), (0, 1, 2));
    assert_eq!(
        row.physical_byte_range_for(&document, authority).unwrap(),
        header.len()..document.byte_len()
    );
    assert_eq!(
        row.physical_utf16_range_for(&document, authority).unwrap(),
        header.len()..document.utf16_len()
    );
    assert_eq!(
        publication
            .table_sources_for(&document, authority)
            .unwrap()
            .windows_for(&document)
            .unwrap()
            .len(),
        0
    );
    assert_eq!(request.total_work().copied_source_bytes, 0);
    assert_eq!(request.total_work().parser_input_bytes, 0);
}

/// Recover the last row through one request, completing all four cells' actual inline semantics without admitting unrelated tail paragraphs.
#[test]
fn narrow_row_source_windows_keep_long_table_forward_and_unrelated_tail_outside() {
    assert_long_table_row_recovery(false);
}

/// A Find selection in the last cell shares the original cumulative budget; successor publications must not repeatedly relocate that distant selection.
#[test]
fn narrow_row_source_windows_keep_last_cell_find_selection_within_original_work_limit() {
    assert_long_table_row_recovery(true);
}

// Both actual selection cases use the same source, four-cell inline preparation, and tail exclusion rather than a lighter Find substitute.
fn assert_long_table_row_recovery(last_cell_find: bool) {
    // Prepare four inline forms from one table source; structural rows alone cannot prove bold, italic, or code-cell readiness.
    let header = "| 普通 | 粗体 | 斜体 | 代码 |\r\n| --- | --- | --- | --- |\r\n";
    let mut source = String::from(header);
    let mut rows = Vec::new();
    let mut next_utf16 = header.encode_utf16().count();
    // Derive expected row coordinates from fixed input, independently of the tested publication and Parser output.
    for ordinal in 1..=240 {
        let row = format!("| 中文{ordinal} | **粗体** | *斜体* | `a\\|b` |\r\n");
        let end = next_utf16 + row.encode_utf16().count();
        rows.push((next_utf16..end, ordinal));
        next_utf16 = end;
        source.push_str(&row);
    }
    // Retain the real tail that an earlier 16K recovery window over-admitted, making accidental source-page growth observable.
    for _ in 0..600 {
        source.push_str("\r\n后方普通段落不能为表格查找预先解析\r\n");
    }
    let (_directory, mut doc) = document(&source);
    if last_cell_find {
        // Select the last row's fixed code literal; the six hundred unrelated tail paragraphs cannot determine this target.
        let byte_start = source.rfind("`a\\|b`").unwrap() + 1;
        let anchor = source[..byte_start].encode_utf16().count();
        doc.set_selection(MarkdownTextSelection {
            anchor_utf16: anchor,
            head_utf16: anchor + "a\\|b".encode_utf16().count(),
        })
        .unwrap();
    }
    let authority = authority(&doc);
    // Publish the actual header first; each later row continues from the latest publication without rebuilding Global or quota.
    let mut request = doc
        .viewport_leaf_source_request(0..1, 41, authority.selection)
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let mut publication = request.take_sources_for(&doc, authority).unwrap().unwrap();
    for (physical, ordinal) in rows {
        // Advance original 4096-unit steps until the complete first-scalar window is ready before changing source coverage.
        let viewport = loop {
            if let Some(viewport) = request
                .table_row_source_viewport(&doc, authority, physical.start, 4096)
                .unwrap()
            {
                break viewport;
            }
        };
        assert_eq!(viewport, physical.start..physical.start + 1);
        request
            .continue_forward(&doc, &publication, viewport.clone(), authority)
            .unwrap();
        finish(&mut request, &doc).unwrap();
        publication = request.take_sources_for(&doc, authority).unwrap().unwrap();
        // A narrow query must still yield the complete physical row identity, not a truncated one-character row.
        assert_eq!(
            publication.requested_for(&doc, authority).unwrap(),
            &viewport
        );
        let row = publication
            .table_row_at_for(&doc, authority, physical.start)
            .unwrap()
            .unwrap();
        assert_eq!(
            row.coordinate_for(&doc, authority).unwrap(),
            (0, ordinal, 4)
        );
        assert_eq!(
            row.physical_utf16_range_for(&doc, authority).unwrap(),
            physical
        );
        // Each cell consumes its original opaque source and semantics, then returns the active slot before the next cell uses this request.
        for column in 0..4 {
            request
                .prepare_cell_inline(&doc, row.clone(), authority, column)
                .unwrap();
            loop {
                if matches!(
                    request.advance_cell_inline(&doc, authority, 4096).unwrap(),
                    MarkdownTableCellInlineStep::Ready { .. }
                ) {
                    break;
                }
            }
            // Consume actual cell semantics before starting the next cell, without reading or mutating the request's private cursors.
            let inline = request
                .take_cell_inline_ready_for(&doc, authority)
                .unwrap()
                .unwrap();
            assert_eq!(
                inline
                    .cell_source_for(&doc, authority)
                    .unwrap()
                    .coordinate_for(&doc)
                    .unwrap()
                    .2,
                column
            );
        }
        // This page serves only the actual table row; unrelated tail paragraphs cannot become hidden prepared inventory.
        assert_eq!(
            publication
                .ordinary_lines_for(&doc, authority)
                .unwrap()
                .count(),
            0
        );
    }
}

/// The fixed Native long-table recipe must produce actual display sources; a longer tail must not merely inflate fuel for an inline-only proof.
#[test]
fn actual_cell_displays_keep_native_long_table_find_within_original_work_limit() {
    use std::io::Write;
    let mut file = tempfile::NamedTempFile::new().unwrap();
    let header = "| 普通 | 粗体 | 斜体 | 行内代码 |\r\n| --- | --- | --- | --- |\r\n";
    file.write_all(header.as_bytes()).unwrap();
    let mut ranges = vec![(0..header.encode_utf16().count(), 0)];
    let mut offset = header.encode_utf16().count();
    let mut selection = MarkdownTextSelection::caret(0);
    for ordinal in 1..=240 {
        let row = format!(
            "| 中文普通{ordinal:04} | **中文粗体{ordinal:04}** | *中文斜体{ordinal:04}* | `中文代码{ordinal:04}` |\r\n"
        );
        if ordinal == 240 {
            let query = format!("中文普通{ordinal:04}");
            let byte = row.find(&query).unwrap();
            let anchor_utf16 = offset + row[..byte].encode_utf16().count();
            selection = MarkdownTextSelection {
                anchor_utf16,
                head_utf16: anchor_utf16 + query.encode_utf16().count(),
            };
        }
        let end = offset + row.encode_utf16().count();
        ranges.push((offset..end, ordinal));
        offset = end;
        file.write_all(row.as_bytes()).unwrap();
    }
    for _ in 0..128 {
        file.write_all("\r\n表后尾部保持按视口读取\r\n".as_bytes())
            .unwrap();
    }
    file.flush().unwrap();
    let mut doc = MarkdownDocumentBuffer::open(file.path(), 32 * 1024 * 1024).unwrap();
    doc.set_selection(selection).unwrap();
    let authority = authority(&doc);
    let mut request = doc
        .viewport_leaf_source_request(0..1, 41, selection)
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let mut publication = request.take_sources_for(&doc, authority).unwrap().unwrap();
    for (physical, ordinal) in ranges {
        if ordinal != 0 {
            // Query this physical row's body as NeedStyle does; CRLF controls retain their own original source handling.
            request
                .continue_forward(
                    &doc,
                    &publication,
                    physical.start..physical.end - 2,
                    authority,
                )
                .unwrap();
            finish(&mut request, &doc).unwrap();
            publication = request.take_sources_for(&doc, authority).unwrap().unwrap();
        }
        let row = publication
            .table_row_at_for(&doc, authority, physical.start)
            .unwrap()
            .unwrap();
        assert_eq!(
            row.coordinate_for(&doc, authority).unwrap(),
            (0, ordinal, 4)
        );
        let mut displays = Vec::new();
        for column in 0..4 {
            // Keep four actual display sources alive together, each prepared through CellInline, scalar conversion, and the disk projection index.
            displays.push(prepare_actual_cell_display(
                &mut request,
                &doc,
                row.clone(),
                authority,
                column,
            ));
        }
        let observers: Vec<_> = displays
            .iter()
            .map(|display| display.resource_observer())
            .collect();
        drop(displays);
        for observer in observers {
            let actual = observer.snapshot();
            assert!(
                actual.source_payload_released
                    && !actual.origin_file_open
                    && actual.origin_buffer_bytes == 0
            );
            assert!(!actual.origin_cleanup_failed && !actual.origin_reader_failed);
        }
    }
}

/// Preparing a wide header again after natural-width measurement must not accumulate already returned semantic workspace in the same quota.
#[test]
fn actual_wide_header_displays_release_serial_preparation_inventory() {
    use std::io::Write;
    let mut file = tempfile::NamedTempFile::new().unwrap();
    let lead = "宽表前的独立正文保持原布局\r\n\r\n";
    for _ in 0..32 {
        file.write_all(lead.as_bytes()).unwrap();
    }
    let header_start = lead.encode_utf16().count() * 32;
    let mut header = String::from("|");
    for column in 1..=20 {
        header.push_str(&format!(
            " 表头{column:02}{}中文 |",
            "宽".repeat(column % 4 + 1)
        ));
    }
    let header_end = header_start + header.encode_utf16().count();
    header.push_str("\r\n|");
    for _ in 0..20 {
        header.push_str(" --- |");
    }
    header.push_str("\r\n");
    file.write_all(header.as_bytes()).unwrap();
    let mut offset = header_start + header.encode_utf16().count();
    let mut target = 0;
    for row in 1..=3 {
        let mut text = String::from("|");
        for column in 1..=20 {
            text.push(' ');
            if row == 1 && column == 1 {
                text.push_str("左端首列目标");
            } else if row == 1 && column == 20 {
                target = offset + text.encode_utf16().count();
                text.push_str("右端末列目标");
            } else {
                text.push_str(&format!("真实第{row:02}行第{column:02}列"));
            }
            text.push_str(" |");
        }
        text.push_str("\r\n");
        offset += text.encode_utf16().count();
        file.write_all(text.as_bytes()).unwrap();
    }
    let tail = "宽表后仍未显示的固定尾段\r\n\r\n";
    for _ in 0..(1024 * 1024) / tail.len() {
        file.write_all(tail.as_bytes()).unwrap();
    }
    file.flush().unwrap();
    let mut doc = MarkdownDocumentBuffer::open(file.path(), 32 * 1024 * 1024).unwrap();
    let selection = MarkdownTextSelection {
        anchor_utf16: target,
        head_utf16: target + "右端末列目标".encode_utf16().count(),
    };
    doc.set_selection(selection).unwrap();
    let authority = authority(&doc);
    let mut request = doc
        .viewport_leaf_source_request(header_start..header_end, 41, selection)
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let publication = request.take_sources_for(&doc, authority).unwrap().unwrap();
    let row = publication
        .table_row_at_for(&doc, authority, header_start)
        .unwrap()
        .unwrap();
    for _ in 0..2 {
        let mut displays = Vec::new();
        for column in 0..20 {
            displays.push(prepare_actual_cell_display(
                &mut request,
                &doc,
                row.clone(),
                authority,
                column,
            ));
        }
        // Each real use retains only the current row; release it before the next pass rather than treating destroyed workspace as live inventory.
        let observers: Vec<_> = displays
            .iter()
            .map(|display| display.resource_observer())
            .collect();
        drop(displays);
        for observer in observers {
            let actual = observer.snapshot();
            assert!(
                actual.source_payload_released
                    && !actual.origin_file_open
                    && actual.origin_buffer_bytes == 0
            );
            assert!(!actual.origin_cleanup_failed && !actual.origin_reader_failed);
        }
    }
}

/// Preserve original first-open/next-close behavior across cells and nested opens; backward recovery rebuilds HTML context from the real root.
#[test]
fn cell_displays_keep_cross_cell_underline_markers_during_forward_and_backward_recovery() {
    // One pair crosses header cells and another crosses into the next row. Preserve nested opens and surplus closes
    // under the existing product grammar rather than hiding them or merging two actual pairs to retain only one item.
    let source = concat!(
        "| <u>头甲 | <u>头乙 </u> | 后丙 </u> | <u>尾丁 |\r\n",
        "| --- | --- | --- | --- |\r\n",
        "| 续甲 | 续乙 </u> | <u>新丙</u> | 末丁 |\r\n",
        "| 后甲 | 后乙 | 后丙 | 后丁 |\r\n",
    );
    let source_len = source.encode_utf16().count();
    let mut row_starts = Vec::new();
    let mut offset = 0;
    for (line, text) in source.split_inclusive('\n').enumerate() {
        if line != 1 {
            row_starts.push(offset);
        }
        offset += text.encode_utf16().count();
    }
    let selected = source[..source.find("头甲").unwrap()]
        .encode_utf16()
        .count();
    for selection in [
        MarkdownTextSelection::caret(source_len),
        MarkdownTextSelection::caret(selected),
    ] {
        let (_directory, mut doc) = document(source);
        doc.set_selection(selection).unwrap();
        let authority = authority(&doc);
        // The complete Parser independently styles this fixed short fixture; expected values do not come from the new scanner or tested output.
        let mut original = crate::MarkdownViewportPresentation::default();
        original.set_active_selection(selection.head_utf16..selection.head_utf16);
        original.rebuild(0, 0..source_len, source).unwrap();
        let mut request = doc
            .viewport_leaf_source_request(0..1, 41, selection)
            .unwrap();
        finish(&mut request, &doc).unwrap();
        let mut publication = request.take_sources_for(&doc, authority).unwrap().unwrap();
        // Return to the completed header and move forward again through real reposition from the latest publication.
        // Do not replace the request to hide an advanced scanner, or reuse stale row tickets across publications.
        for (step, row_index) in [0, 1, 2, 0, 1].into_iter().enumerate() {
            let row_start = row_starts[row_index];
            if step != 0 {
                if row_index == 0 {
                    request
                        .reposition(&doc, &publication, row_start..row_start + 1, authority)
                        .unwrap();
                } else {
                    request
                        .continue_forward(&doc, &publication, row_start..row_start + 1, authority)
                        .unwrap();
                }
                finish(&mut request, &doc).unwrap();
                publication = request.take_sources_for(&doc, authority).unwrap().unwrap();
            }
            let row = publication
                .table_row_at_for(&doc, authority, row_start)
                .unwrap()
                .unwrap();
            assert_eq!(
                row.coordinate_for(&doc, authority).unwrap(),
                (0, row_index, 4)
            );
            for column in 0..4 {
                request
                    .prepare_cell_inline(&doc, row.clone(), authority, column)
                    .unwrap();
                loop {
                    if matches!(
                        request.advance_cell_inline(&doc, authority, 4096).unwrap(),
                        MarkdownTableCellInlineStep::Ready { .. }
                    ) {
                        break;
                    }
                }
                let inline = request
                    .take_cell_inline_ready_for(&doc, authority)
                    .unwrap()
                    .unwrap();
                let raw = inline
                    .cell_source_for(&doc, authority)
                    .unwrap()
                    .canonical_range_for(&doc)
                    .unwrap();
                for index in raw.clone() {
                    let actual = inline.query_for(&doc, authority, index).unwrap().unwrap();
                    let expected = original.query(index).unwrap();
                    // Table/header inheritance has its own source; compare the three tag roles affected by this migration.
                    for role in [
                        MarkdownPresentationStyles::UNDERLINE,
                        MarkdownPresentationStyles::MARKER,
                        MarkdownPresentationStyles::HIDDEN_MARKER,
                    ] {
                        assert_eq!(
                            actual.styles.contains(role),
                            expected.styles.contains(role),
                            "row={row_index} column={column} index={index} role={role:?} selection={selection:?}"
                        );
                    }
                }
                drop(inline);
                // Preparing display for the same coverage must return all original pairs and acquire actual semantics again.
                // Then create and release the disk projection; query results alone cannot prove the Display lifecycle.
                let display =
                    prepare_actual_cell_display(&mut request, &doc, row.clone(), authority, column);
                let observer = display.resource_observer();
                drop(display);
                let released = observer.snapshot();
                assert!(released.source_payload_released && !released.origin_file_open);
                assert_eq!(released.origin_buffer_bytes, 0);
                assert!(!released.origin_cleanup_failed && !released.origin_reader_failed);
            }
        }
    }
}

/// Use public production preparation to return an actual completed display Source; raw/inline readiness cannot substitute for its finished index.
fn prepare_actual_cell_display(
    request: &mut MarkdownViewportSourceRequest,
    document: &MarkdownDocumentBuffer,
    row: VerifiedMarkdownTableRowSource,
    authority: MarkdownViewportSourceAuthority,
    column: usize,
) -> crate::MarkdownTableCellDisplaySource {
    let coordinate = row.coordinate_for(document, authority).unwrap();
    let mut preparation = crate::MarkdownTableCellDisplayPreparation::from_row_source(
        document, row, authority, column,
    )
    .unwrap();
    loop {
        let step = preparation
            .advance_with_source_request(document, request, false)
            .unwrap_or_else(|cause| {
                panic!("实际显示准备失败 row={coordinate:?} column={column} cause={cause:?}")
            });
        if matches!(
            step,
            crate::MarkdownTableCellDisplayPreparationStep::Ready { .. }
        ) {
            break;
        }
    }
    let display = preparation.take_ready_for(document).unwrap().unwrap();
    assert_eq!(
        display
            .canonical_raw_cell()
            .coordinate_for(document)
            .unwrap()
            .2,
        column
    );
    assert!(display.source().utf16_len() > 0);
    display
}

/// A row without a leading pipe can begin with a supplementary scalar; its first-scalar window must include both UTF-16 units.
#[test]
fn row_source_window_preserves_supplementary_scalar_and_rejects_surrogate_interior() {
    let source = "H | C\r\n--- | ---\r\n😀原格 | 中文\r\n";
    let (_directory, doc) = document(source);
    let authority = authority(&doc);
    let row_start = source[..source.find('😀').unwrap()].encode_utf16().count();
    // Omit a leading pipe so the actual row starts with a two-unit scalar, preventing an ASCII prefix from making the case trivial.
    let mut request = doc
        .viewport_leaf_source_request(0..1, 41, authority.selection)
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let initial = request.take_sources_for(&doc, authority).unwrap().unwrap();
    // The original locator proves the scalar window; its expected length comes from the fixed character rather than the returned value.
    let viewport = loop {
        if let Some(viewport) = request
            .table_row_source_viewport(&doc, authority, row_start, 4096)
            .unwrap()
        {
            break viewport;
        }
    };
    assert_eq!(viewport, row_start..row_start + 2);
    request
        .continue_forward(&doc, &initial, viewport, authority)
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let row_sources = request.take_sources_for(&doc, authority).unwrap().unwrap();
    let row = row_sources
        .table_row_at_for(&doc, authority, row_start)
        .unwrap()
        .unwrap();
    assert_eq!(row.coordinate_for(&doc, authority).unwrap(), (0, 1, 2));
    // A low-surrogate position is not another valid row start; preserve failure instead of flooring it into fabricated authority.
    let failure = loop {
        match request.table_row_source_viewport(&doc, authority, row_start + 1, 4096) {
            Ok(None) => {}
            result => break result,
        }
    };
    assert_eq!(failure, Err(MarkdownTableRequestError::InvalidRequest));
}
use pulldown_cmark::{Event, Parser, Tag, TagEnd};

fn document(source: &str) -> (tempfile::TempDir, MarkdownDocumentBuffer) {
    // TempDir owns only this synthetic file; retain it alongside the actual public document buffer until each case ends.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("普通来源验收.md");
    std::fs::write(&path, source).unwrap();
    let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap();
    (directory, document)
}

fn authority(document: &MarkdownDocumentBuffer) -> MarkdownViewportSourceAuthority {
    MarkdownViewportSourceAuthority {
        context_revision: 41,
        selection: document.selection(),
        cancelled: false,
    }
}

fn finish(
    request: &mut MarkdownViewportSourceRequest,
    document: &MarkdownDocumentBuffer,
) -> Result<(), MarkdownTableRequestError> {
    // Drive the existing request rather than another parsing path; this finite test loop observes each field's separate step bound.
    for _ in 0..document
        .byte_len()
        .saturating_mul(100)
        .saturating_add(10000)
    {
        match request.advance(document, authority(document), 4096)? {
            MarkdownViewportSourceStep::Progress { work }
            | MarkdownViewportSourceStep::SourcesAvailable { work } => {
                assert!(work.source_bytes <= 4096);
                assert!(work.structural_transitions <= 4096);
                assert!(work.metadata_bytes <= 4096);
            }
        }
        if request
            .sources_for(document, authority(document))?
            .is_some()
        {
            return Ok(());
        }
    }
    panic!("有限真实来源夹具没有结束");
}

fn units(source: &str, byte: usize) -> usize {
    source[..byte].encode_utf16().count()
}

fn ordinary_text_ranges(source: &str) -> Vec<Range<usize>> {
    let options =
        crate::markdown_viewport_presentation::parser::markdown_presentation_options(true);
    let mut table = false;
    let mut code = false;
    let mut atx = false;
    let mut ranges = Vec::new();
    for (event, range) in Parser::new_ext(source, options).into_offset_iter() {
        // These finite fixtures omit inline markup so original Text offsets independently constrain physical content boundaries.
        match event {
            Event::Start(Tag::Table(_)) => table = true,
            Event::End(TagEnd::Table) => table = false,
            Event::Start(Tag::CodeBlock(_)) => code = true,
            Event::End(TagEnd::CodeBlock) => code = false,
            Event::Start(Tag::Heading { .. }) => atx = source.as_bytes()[range.start] == b'#',
            Event::End(TagEnd::Heading(_)) => atx = false,
            Event::Text(_) if !table && !code && !atx => ranges.push(range),
            _ => (), // HTML, markers, and breaks do not belong to this ordinary-content oracle.
        }
    }
    ranges
}

fn assert_line_oracle(
    source: &str,
    lines: &[&VerifiedMarkdownOrdinaryLine],
    expected: &[Range<usize>],
) {
    assert_eq!(lines.len(), expected.len());
    for (actual, content) in lines.iter().zip(expected) {
        // Find actual CR/LF in the input bytes rather than deriving expectations from the new origin output.
        let physical = source.as_bytes()[..content.start]
            .iter()
            .rposition(|byte| matches!(byte, b'\r' | b'\n'))
            .map_or(0, |byte| byte + 1);
        let content_end = source.as_bytes()[content.start..]
            .iter()
            .position(|byte| matches!(byte, b'\r' | b'\n'))
            .map_or(source.len(), |offset| content.start + offset);
        assert_eq!(
            content.end, content_end,
            "本 oracle 夹具没有行尾空白或 inline markup"
        );
        let eol_end = match source.as_bytes().get(content_end..) {
            Some([b'\r', b'\n', ..]) => content_end + 2,
            Some([b'\r' | b'\n', ..]) => content_end + 1,
            _ => content_end,
        };
        assert_eq!(actual.origin().physical_range(), physical..eol_end);
        assert_eq!(actual.origin().prefix_range(), physical..content.start);
        assert_eq!(actual.origin().content_range(), content.clone());
        assert_eq!(actual.origin().eol_range(), content_end..eol_end);
        assert_eq!(
            actual.physical_utf16_range(),
            units(source, physical)..units(source, eol_end)
        );
        assert_eq!(
            actual.prefix_utf16_range(),
            units(source, physical)..units(source, content.start)
        );
        assert_eq!(
            actual.content_utf16_range(),
            units(source, content.start)..units(source, content_end)
        );
        assert_eq!(
            actual.eol_utf16_range(),
            units(source, content_end)..units(source, eol_end)
        );
    }
}

#[test]
// Reposition replays the same actual Global, preserves old read-only publications, and charges replay to original cumulative work.
fn root_leaf_reposition_replays_same_global_keeps_old_permissions_and_cumulative_work() {
    let source = format!("# 开头\n\n{}\n# 结尾\n", "中文 ascii\n".repeat(1200));
    let (_directory, doc) = document(&source);
    let tail = doc.utf16_len() - 6..doc.utf16_len();
    let mut request = doc
        .viewport_leaf_source_request(tail.clone(), 41, doc.selection())
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let old = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let original_global = request.batch.source_probe_state().0;
    let original_work = request.total_work();
    request
        .reposition(&doc, &old, 0..4, authority(&doc))
        .unwrap();
    assert_eq!(request.batch.source_probe_state().0, original_global);
    assert_eq!(request.batch.source_probe_state().1, 0);
    assert!(request.total_work().structural_transitions > original_work.structural_transitions);
    finish(&mut request, &doc).unwrap();
    let first = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let MarkdownOrdinarySemanticState::RootLeafReady(leaf) =
        first.ordinary_semantics_for(&doc, authority(&doc)).unwrap()
    else {
        panic!("重放文首必须取得真实RootLeaf权限");
    };
    assert!(leaf.query_for(&doc, authority(&doc), 2).unwrap().is_some());
    old.ordinary_semantics_for(&doc, authority(&doc)).unwrap(); // Reposition does not replace the old immutable publication's identity or selection.
    request
        .continue_forward(&doc, &first, tail, authority(&doc))
        .unwrap();
    finish(&mut request, &doc).unwrap();
    assert_eq!(request.batch.source_probe_state().0, original_global);
    assert!(request.total_work().source_bytes > original_work.source_bytes);
    assert_eq!(request.total_work().copied_source_bytes, 0);
    request.cancel();
}

#[test]
// An unrelated publication fails before mutation, while cancellation actually retires the request instead of leaving a resumable frontier.
fn root_leaf_reposition_rejects_foreign_publication_without_destroying_original_owner() {
    let (_directory, doc) = document("# 头\n\n正文\n\n# 尾\n");
    let mut request = doc
        .viewport_leaf_source_request(6..doc.utf16_len(), 41, doc.selection())
        .unwrap();
    let mut foreign = doc
        .viewport_leaf_source_request(0..3, 41, doc.selection())
        .unwrap();
    finish(&mut request, &doc).unwrap();
    finish(&mut foreign, &doc).unwrap();
    let old = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let other = foreign
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let state = request.batch.source_probe_state();
    assert_eq!(
        request.reposition(&doc, &other, 0..3, authority(&doc)),
        Err(MarkdownTableRequestError::PublicationSuperseded)
    );
    assert_eq!(request.batch.source_probe_state(), state);
    request
        .reposition(&doc, &old, 0..3, authority(&doc))
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let cancelled = MarkdownViewportSourceAuthority {
        cancelled: true,
        ..authority(&doc)
    };
    let ready = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    assert_eq!(
        request.reposition(&doc, &ready, 4..8, cancelled),
        Err(MarkdownTableRequestError::Cancelled)
    );
    assert_eq!(request.batch.source_probe_state().0, 0); // Actual cancellation releases the original Global; a new range cannot revive this owner.
}

#[test]
// Compare original physical line/leaf facts with a complete finite Parser oracle while checking their shared metadata adds no strong root owner.
fn mixed_real_ordinary_table_and_leaf_sources_match_full_parser_and_physical_bytes() {
    let source = "前段😀\r\n续行\r\n\r\n> 引用一\r\n> 引用二\r\n\r\n- [x] 任务一\n  任务二\n\n| H | C |\n| - | - |\n| a | b |\n\n# 原生标题\n\n```rust\nlet value = 1;\n```\n\nSetext 中\r\n===\r\n\r\n末段";
    let (_directory, doc) = document(source);
    let owners = Arc::strong_count(doc.root.as_ref().unwrap());
    let mut request = doc
        .viewport_source_request(0..doc.utf16_len(), 41, doc.selection())
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let sources = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let lines = sources
        .ordinary_lines_for(&doc, authority(&doc))
        .unwrap()
        .collect::<Vec<_>>();
    assert_line_oracle(source, &lines, &ordinary_text_ranges(source));
    let leaves = sources
        .ordinary_leaves_for(&doc, authority(&doc))
        .unwrap()
        .collect::<Vec<_>>();
    assert!(
        leaves
            .iter()
            .any(|leaf| matches!(leaf.kind(), LeafBlockKind::AtxHeading(_)))
    );
    assert!(
        leaves
            .iter()
            .any(|leaf| leaf.kind() == LeafBlockKind::FencedCode)
    );
    assert_eq!(
        sources
            .table_sources_for(&doc, authority(&doc))
            .unwrap()
            .windows_for(&doc)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(Arc::strong_count(doc.root.as_ref().unwrap()), owners);
    assert!(
        request.total_work().source_bytes > source.len(),
        "结构、来源和坐标真实重读均须入账"
    );
    assert_eq!(
        request.total_work().copied_source_bytes,
        sources
            .table_sources_for(&doc, authority(&doc))
            .unwrap()
            .budget()
            .source_bytes,
        "ordinary 元数据不能另建正文 String；只有原有限 table 输入允许复制"
    );
}

#[test]
// Overlapping forward coverage within one unfinished paragraph must continue original origin work rather than restarting classification.
fn middle_of_long_paragraph_keeps_same_unfinished_origin_and_global_for_forward_extension() {
    let source = (0..1200)
        .map(|line| format!("第{line}行中😀\r\n"))
        .collect::<String>();
    let starts = source
        .match_indices('第')
        .map(|(byte, _)| byte)
        .collect::<Vec<_>>();
    let (_directory, doc) = document(&source);
    let viewport = units(&source, starts[400])..units(&source, starts[405]);
    let mut request = doc
        .viewport_source_request(viewport.clone(), 41, doc.selection())
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let old = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let global = request.batch.source_probe_state().0;
    let cursor = match request.job.as_ref().unwrap() {
        OrdinaryJob::Inline(job) => std::ptr::from_ref(job.cursor.as_ref()).addr(),
        OrdinaryJob::Leaf(_) => panic!("该真实夹具只有一个 paragraph"),
    };
    assert_eq!(request.origin_forward_floor(), viewport.end);
    assert!(
        request.batch.forward_resume_floor() > request.origin_forward_floor(),
        "分类水位不能代替已消费来源"
    );
    let old_work = request.total_work();
    let next = units(&source, starts[403])..units(&source, starts[411]);
    request
        .continue_forward(&doc, &old, next.clone(), authority(&doc))
        .unwrap();
    assert_eq!(request.batch.source_probe_state().0, global);
    assert_eq!(
        match request.job.as_ref().unwrap() {
            OrdinaryJob::Inline(job) => std::ptr::from_ref(job.cursor.as_ref()).addr(),
            _ => 0,
        },
        cursor
    );
    finish(&mut request, &doc).unwrap();
    let sources = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let expected = ordinary_text_ranges(&source)
        .into_iter()
        .filter(|range| {
            let start = units(&source, range.start);
            next.start <= start && start < next.end
        })
        .collect::<Vec<_>>();
    let lines = sources
        .ordinary_lines_for(&doc, authority(&doc))
        .unwrap()
        .collect::<Vec<_>>();
    assert_line_oracle(&source, &lines, &expected);
    assert_eq!(request.batch.source_probe_state().0, global);
    assert!(request.total_work().source_bytes > old_work.source_bytes);
    assert!(
        request.total_work().source_bytes - old_work.source_bytes < source.len() / 4,
        "同一未完 cursor 前进不能重扫原 Global 或已处理的段前缀"
    );
    assert!(
        old.ordinary_lines_for(&doc, authority(&doc))
            .unwrap()
            .next()
            .is_some(),
        "旧 immutable 来源仍可读"
    );
    assert_eq!(
        request
            .continue_forward(&doc, &old, next.clone(), authority(&doc))
            .unwrap_err(),
        MarkdownTableRequestError::PublicationSuperseded
    );
    assert_eq!(
        request
            .continue_forward(&doc, &sources, viewport, authority(&doc))
            .unwrap_err(),
        MarkdownTableRequestError::CoverageRequiresCheckpoint
    );
}

#[test]
// Continuous byte-to-UTF-16 conversion must cross actual line/block gaps once, including empty lines and supplementary scalars.
fn continuous_coordinates_visit_short_line_bytes_once_across_paragraph_boundaries() {
    let source = "中😀\r\n\r\n短\n".repeat(80);
    let (_directory, doc) = document(&source);
    let mut boundaries = Vec::new();
    let mut start = 0;
    while start < source.len() {
        let end = source.as_bytes()[start..]
            .iter()
            .position(|byte| matches!(byte, b'\r' | b'\n'))
            .map_or(source.len(), |length| start + length);
        let eol_end = if source.as_bytes().get(end..end + 2) == Some(b"\r\n") {
            end + 2
        } else {
            (end + 1).min(source.len())
        };
        boundaries.push([start, start, end, eol_end]);
        start = eol_end;
    }
    let (mut coordinates, initial) = Coordinates::new(&doc, boundaries[0], 4096).unwrap();
    assert!(initial.source_tree_steps > 0 && initial.source_bytes == 0);
    let mut bytes = 0;
    for (index, boundary) in boundaries.into_iter().enumerate() {
        if index != 0 {
            let reset = coordinates.continue_at(boundary).unwrap();
            assert_eq!(reset.source_tree_steps, 0, "续行不能重新定位相同 leaf");
            assert_eq!(reset.source_bytes, 0);
        }
        loop {
            let (work, values) = coordinates.advance(&doc, 4096).unwrap();
            bytes += work.source_bytes;
            if let Some(values) = values {
                assert_eq!(values, boundary.map(|byte| units(&source, byte)));
                break;
            }
        }
    }
    assert_eq!(bytes, source.len(), "不能逐条 origin 从同 leaf 起点重扫");
}

#[test]
// A giant physical line has one origin descriptor; scalar conversion remains incremental without copying its full text.
fn giant_unicode_line_has_one_origin_record_and_bounded_real_coordinate_steps() {
    let source = "中😀e\u{301}".repeat(10000);
    let (_directory, doc) = document(&source);
    let viewport = 10000..10100;
    let mut request = doc
        .viewport_source_request(viewport, 41, doc.selection())
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let sources = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let lines = sources
        .ordinary_lines_for(&doc, authority(&doc))
        .unwrap()
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].origin().content_range(), 0..source.len());
    assert_eq!(lines[0].content_utf16_range(), 0..doc.utf16_len());
    assert_eq!(request.total_work().copied_source_bytes, 0);
    assert_eq!(request.total_work().allocated_source_capacity, 0);
    // Different bridge budgets force frames to end inside scalars while total actual reads still equal the source length.
    for budget in [2048, 2051, 4096] {
        let (mut coordinates, _) =
            Coordinates::new(&doc, [0, 0, source.len(), source.len()], budget).unwrap();
        let mut bytes = 0;
        loop {
            let (work, values) = coordinates.advance(&doc, budget).unwrap();
            bytes += work.source_bytes;
            assert!(work.source_bytes <= budget && work.structural_transitions <= budget);
            if let Some(values) = values {
                assert_eq!(values, [0, 0, doc.utf16_len(), doc.utf16_len()]);
                break;
            }
        }
        assert_eq!(bytes, source.len());
    }
}

#[test]
// Repeated successful publications cannot renew this request's transition total; exhaustion leaves the last exported result independently readable.
fn repeated_real_source_publications_exhaust_original_total_fuel_without_reset() {
    let source = "one\ntwo\n";
    let (_directory, doc) = document(source);
    let viewport = 0..doc.utf16_len();
    let mut request = doc
        .viewport_source_request(viewport.clone(), 41, doc.selection())
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let mut previous = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let initial = request.total_work();
    let mut exhausted = false;
    for _ in 0..10000 {
        let continued =
            request.continue_forward(&doc, &previous, viewport.clone(), authority(&doc));
        if continued == Err(MarkdownTableRequestError::WorkLimit) {
            exhausted = true;
            break;
        }
        continued.unwrap();
        match finish(&mut request, &doc) {
            Err(MarkdownTableRequestError::WorkLimit) => {
                exhausted = true;
                break;
            }
            Ok(()) => {
                previous = request
                    .take_sources_for(&doc, authority(&doc))
                    .unwrap()
                    .unwrap()
            }
            Err(error) => panic!("实际有限发布出现其它失败 {error:?}"),
        }
    }
    assert!(
        exhausted,
        "新 publication 不能重新取得原请求的总 transitions fuel"
    );
    assert!(request.total_work().structural_transitions > initial.structural_transitions);
    assert_eq!(
        previous
            .ordinary_lines_for(&doc, authority(&doc))
            .unwrap()
            .count(),
        2
    );
    assert_eq!(
        request.advance(&doc, authority(&doc), 4096).unwrap_err(),
        MarkdownTableRequestError::WorkLimit
    );
}

#[test]
// Context revision and directed selection are independent authority; restoring old values cannot revive an already failed mutable owner.
fn context_and_full_reverse_selection_are_checked_and_mutating_failure_is_sticky() {
    let source = "first\nsecond\n\nthird\n";
    let (_directory, mut doc) = document(source);
    let selection = MarkdownTextSelection {
        anchor_utf16: 1,
        head_utf16: 4,
    };
    doc.set_selection(selection).unwrap();
    for context_change in [false, true] {
        let mut request = doc
            .viewport_source_request(0..doc.utf16_len(), 41, selection)
            .unwrap();
        let mut wrong = authority(&doc);
        if context_change {
            wrong.context_revision += 1;
        } else {
            wrong.selection = MarkdownTextSelection {
                anchor_utf16: 4,
                head_utf16: 1,
            };
            doc.set_selection(wrong.selection).unwrap();
        }
        assert_eq!(
            request.advance(&doc, wrong, 4096).unwrap_err(),
            MarkdownTableRequestError::PublicationSuperseded
        );
        doc.set_selection(selection).unwrap();
        assert_eq!(
            request.advance(&doc, authority(&doc), 4096).unwrap_err(),
            MarkdownTableRequestError::PublicationSuperseded
        );
        assert!(request.job.is_none() && request.coordinates.is_none());
    }
    let mut request = doc
        .viewport_source_request(0..doc.utf16_len(), 41, selection)
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let sources = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let mut wrong = authority(&doc);
    wrong.context_revision += 1;
    assert!(matches!(
        sources.ordinary_lines_for(&doc, wrong),
        Err(MarkdownTableRequestError::PublicationSuperseded)
    ));
    let mut cancelled = authority(&doc);
    cancelled.cancelled = true;
    assert_eq!(
        request
            .continue_forward(&doc, &sources, 0..doc.utf16_len(), cancelled)
            .unwrap_err(),
        MarkdownTableRequestError::Cancelled
    );
    assert_eq!(
        request.advance(&doc, authority(&doc), 4096).unwrap_err(),
        MarkdownTableRequestError::Cancelled
    );
    assert!(request.job.is_none() && request.coordinates.is_none());
    assert!(
        sources
            .ordinary_lines_for(&doc, authority(&doc))
            .unwrap()
            .next()
            .is_some()
    );
}

#[test]
// Cancelling transfer must release the pending result and prevent a later take with valid-looking authority from reviving it.
fn cancelled_take_releases_pending_payload_and_cannot_be_revived_by_correct_authority() {
    let source = "first\nsecond\nthird\n";
    let (_directory, doc) = document(source);
    let mut request = doc
        .viewport_source_request(0..doc.utf16_len(), 41, doc.selection())
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let mut cancelled = authority(&doc);
    cancelled.cancelled = true;
    assert_eq!(
        request.take_sources_for(&doc, cancelled).err(),
        Some(MarkdownTableRequestError::Cancelled)
    );
    assert!(request.ready.is_none() && request.job.is_none() && request.coordinates.is_none());
    assert_eq!(
        request.take_sources_for(&doc, authority(&doc)).err(),
        Some(MarkdownTableRequestError::Cancelled)
    );
    assert_eq!(
        request.advance(&doc, authority(&doc), 4096).unwrap_err(),
        MarkdownTableRequestError::Cancelled
    );
}

#[test]
// Another note with equal bytes and Undo to the same tree remain distinct source authorities because identity and generation are both required.
fn edit_undo_same_root_new_generation_and_other_document_cannot_reuse_sources() {
    let source = "中文😀\nsecond\n\nthird";
    let (_directory, mut doc) = document(source);
    let (_other_directory, other) = document(source);
    let mut request = doc
        .viewport_source_request(0..doc.utf16_len(), 41, doc.selection())
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let sources = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    assert!(matches!(
        sources.ordinary_lines_for(&other, authority(&other)),
        Err(MarkdownTableRequestError::SourceSuperseded)
    ));
    let root = Arc::downgrade(doc.root.as_ref().unwrap());
    let generation = doc.generation();
    doc.insert_utf16(0, "插入", MarkdownTextSelection::caret(2))
        .unwrap();
    doc.undo().unwrap();
    assert!(std::sync::Weak::ptr_eq(
        &root,
        &Arc::downgrade(doc.root.as_ref().unwrap())
    ));
    assert!(doc.generation() > generation);
    assert!(matches!(
        sources.ordinary_lines_for(&doc, authority(&doc)),
        Err(MarkdownTableRequestError::SourceSuperseded)
    ));
    assert_eq!(
        request.advance(&doc, authority(&doc), 4096).unwrap_err(),
        MarkdownTableRequestError::SourceSuperseded
    );
    assert!(request.job.is_none() && request.coordinates.is_none());
}

#[test]
// Synchronous coordinate setup and later publication are separate actual work boundaries, including cancellation immediately after location.
fn publication_is_a_separate_counted_step_and_coordinate_setup_is_counted_before_cancel() {
    let source = "first\nsecond\nthird\n";
    let (_directory, doc) = document(source);
    let mut request = doc
        .viewport_source_request(0..doc.utf16_len(), 41, doc.selection())
        .unwrap();
    let mut saw_coordinates = false;
    for _ in 0..10000 {
        let before = request.total_work();
        request.advance(&doc, authority(&doc), 4096).unwrap();
        if request.coordinates.is_some() && request.coordinate_active {
            let after = request.total_work();
            assert!(
                after.source_tree_steps > before.source_tree_steps,
                "首次 Coordinates::new 的真实树定位必须当步归账"
            );
            request.cancel();
            assert!(request.coordinates.is_none() && request.job.is_none());
            saw_coordinates = true;
            break;
        }
    }
    assert!(saw_coordinates);
    let mut request = doc
        .viewport_source_request(0..doc.utf16_len(), 41, doc.selection())
        .unwrap();
    for _ in 0..10000 {
        request.advance(&doc, authority(&doc), 4096).unwrap();
        if request.batch.ready_for(&doc).unwrap().is_some() {
            assert!(
                request.ready.is_none(),
                "batch Ready 的尾部不免费创建来源 publication"
            );
            let before = request.total_work();
            let step = request.advance(&doc, authority(&doc), 4096).unwrap();
            let MarkdownViewportSourceStep::SourcesAvailable { work } = step else {
                panic!("独立发布步应完成")
            };
            assert!(work.structural_transitions >= 2 * CONTROL_COST);
            assert!(work.metadata_bytes >= publication_metadata());
            assert_eq!(
                request.total_work().structural_transitions - before.structural_transitions,
                work.structural_transitions
            );
            assert_eq!(
                request.total_work().metadata_bytes - before.metadata_bytes,
                work.metadata_bytes
            );
            return;
        }
    }
    panic!("未经过显式来源发布边界");
}

#[test]
// A successor shares quota with still-live previous records; finite page failure retires only current work, not the old immutable directory.
fn source_records_are_bounded_without_losing_the_original_immutable_publication() {
    let source = (0..300)
        .map(|line| format!("x{line}\n"))
        .collect::<String>();
    let starts = source
        .match_indices('x')
        .map(|(byte, _)| byte)
        .collect::<Vec<_>>();
    let (_directory, doc) = document(&source);
    let mut request = doc
        .viewport_source_request(0..starts[100], 41, doc.selection())
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let previous = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    request
        .continue_forward(&doc, &previous, starts[90]..starts[200], authority(&doc))
        .unwrap();
    let metadata_before = request.batch.source_probe_state().2;
    assert!(
        metadata_before
            >= previous.retained_metadata + source_workspace_metadata() + publication_metadata(),
        "旧来源记录仍活时先联合预扣，不能修剪后才退款"
    );
    finish(&mut request, &doc).unwrap();
    let next = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    assert_eq!(
        next.ordinary_lines_for(&doc, authority(&doc))
            .unwrap()
            .count(),
        110
    );
    assert!(request.batch.source_probe_state().2 >= metadata_before + 100 * record_metadata());
    assert_eq!(
        previous
            .ordinary_lines_for(&doc, authority(&doc))
            .unwrap()
            .count(),
        100
    );
    // A page over limit returns a typed failure; source fallback or partial ordinary readiness cannot hide missing lines.
    let mut excessive = doc
        .viewport_source_request(0..doc.utf16_len(), 41, doc.selection())
        .unwrap();
    assert_eq!(
        finish(&mut excessive, &doc).unwrap_err(),
        MarkdownTableRequestError::Window(MarkdownTableWindowError::SemanticLimit)
    );
    assert!(
        excessive.records.is_empty() && excessive.job.is_none() && excessive.coordinates.is_none()
    );
    assert_eq!(
        excessive.advance(&doc, authority(&doc), 4096).unwrap_err(),
        MarkdownTableRequestError::Window(MarkdownTableWindowError::SemanticLimit)
    );
}

fn table_source(rows: usize, columns: usize) -> String {
    let mut source = String::new();
    source.push_str(&format!("|{}\n", " H |".repeat(columns)));
    source.push_str(&format!("|{}\n", " - |".repeat(columns)));
    for _ in 0..rows {
        source.push_str(&format!("|{}\n", " x |".repeat(columns)));
    }
    source
}

#[test]
// A forward jump must admit the old large table and new table together before publishing, even when the latter is independently legal.
fn forward_jump_still_admits_live_previous_table_inventory_before_new_table() {
    let old = table_source(60, 16);
    let later = table_source(4, 16);
    let source = format!("{old}\n间隔\n\n{later}");
    let later_start = source.len() - later.len();
    let (_directory, doc) = document(&source);
    let mut request = doc
        .viewport_source_request(0..old.len(), 41, doc.selection())
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let previous = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    assert!(previous.table.budget().cells >= 960);
    assert!(units(&source, later_start) > request.batch.forward_resume_floor());
    let viewport = units(&source, later_start)..doc.utf16_len();
    request
        .continue_forward(&doc, &previous, viewport.clone(), authority(&doc))
        .unwrap();
    assert_eq!(
        finish(&mut request, &doc).unwrap_err(),
        MarkdownTableRequestError::Window(MarkdownTableWindowError::CellLimit)
    );
    assert_eq!(
        previous
            .table_sources_for(&doc, authority(&doc))
            .unwrap()
            .windows_for(&doc)
            .unwrap()
            .len(),
        1
    );
    // The new table is independently valid; rejection must come from its coexistence with the old large table, not malformed input.
    let mut independent = doc
        .viewport_source_request(viewport, 41, doc.selection())
        .unwrap();
    finish(&mut independent, &doc).unwrap();
    assert_eq!(
        independent
            .sources_for(&doc, authority(&doc))
            .unwrap()
            .unwrap()
            .table
            .windows_for(&doc)
            .unwrap()
            .len(),
        1
    );
}

#[test]
// Retained source descriptions and shared scope metadata must not keep the actual document root alive after its owner closes.
fn request_and_sources_never_keep_document_root_alive_after_close_owner_drop() {
    let source = "> first😀\r\n> second\r\n\r\n# leaf\r\n";
    let (_directory, doc) = document(source);
    let root = Arc::downgrade(doc.root.as_ref().unwrap());
    let mut request = doc
        .viewport_source_request(0..doc.utf16_len(), 41, doc.selection())
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let sources = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    drop(doc);
    assert_eq!(
        root.strong_count(),
        0,
        "来源及 shared scope 页不能强持 piece root/mmap"
    );
    assert!(!sources.records.is_empty());
    request.cancel();
    assert!(request.records.is_empty() && request.coordinates.is_none() && request.job.is_none());
    drop(sources);
}
