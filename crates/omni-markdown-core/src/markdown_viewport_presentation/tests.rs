//! Regression tests preserved with their owning document/workspace boundary.

use super::{
    MarkdownPresentationMode, MarkdownPresentationStyles, MarkdownTableAlignment,
    MarkdownViewportPresentation,
};

/// Build a current bounded presentation fixture with explicit absolute origin and selection.
fn rebuilt_presentation(
    source: &str,
    absolute_start: usize,
    active_selection: std::ops::Range<usize>,
) -> MarkdownViewportPresentation {
    // Compute the exact UTF-16 window from fixture text and parse it under one known generation.
    let mut presentation = MarkdownViewportPresentation::default();
    presentation.set_active_selection(active_selection);
    let utf16_end = absolute_start + source.encode_utf16().count();
    presentation
        .rebuild(7, absolute_start..utf16_end, source)
        .expect("build bounded Markdown presentation");
    presentation
}

/// Translate fixture byte locations through an independent scalar-boundary oracle.
fn utf16_offset_at_byte(source: &str, byte_offset: usize) -> usize {
    // Assert the fixture location is a scalar boundary before counting its UTF-16 prefix.
    assert!(source.is_char_boundary(byte_offset));
    source[..byte_offset].encode_utf16().count()
}

#[test]
/// Protect all supported semantic style families in one representative Markdown envelope.
fn commonmark_gfm_and_confirmed_extensions_receive_semantic_styles() {
    // Mix confirmed block and inline extensions, then combine the produced style bits.
    let source = [
        "# Heading",
        "",
        "**bold** *emphasis* ~~strike~~ `code` [link](https://example.test)",
        "> quote",
        "- bullet",
        "1. ordered",
        "- [ ] task",
        "---",
        "```rust",
        "fn main() {}",
        "```",
        "| Name | Value |",
        "| --- | ---: |",
        "| Omni | Fast |",
        "$x + y$ ^sup^ <u>underlined</u>",
    ]
    .join("\n");
    let presentation = rebuilt_presentation(&source, 0, source.encode_utf16().count()..0);
    let styles =
        presentation
            .runs
            .iter()
            .fold(MarkdownPresentationStyles::default(), |combined, run| {
                // Combine independent style roles so the fixture can assert support for every enabled extension.
                combined.union(run.styles)
            });

    // Each supported role must appear independently so a missing parser option cannot silently drop it.
    for expected in [
        MarkdownPresentationStyles::HEADING_1,
        MarkdownPresentationStyles::STRONG,
        MarkdownPresentationStyles::EMPHASIS,
        MarkdownPresentationStyles::STRIKETHROUGH,
        MarkdownPresentationStyles::INLINE_CODE,
        MarkdownPresentationStyles::LINK,
        MarkdownPresentationStyles::BLOCK_QUOTE,
        MarkdownPresentationStyles::UNORDERED_LIST,
        MarkdownPresentationStyles::ORDERED_LIST,
        MarkdownPresentationStyles::TASK_LIST,
        MarkdownPresentationStyles::HORIZONTAL_RULE,
        MarkdownPresentationStyles::FENCED_CODE,
        MarkdownPresentationStyles::TABLE,
        MarkdownPresentationStyles::FORMULA,
        MarkdownPresentationStyles::SUPERSCRIPT,
        MarkdownPresentationStyles::UNDERLINE,
    ] {
        // Check each independent feature bit rather than allowing a nonempty aggregate to hide a missing extension.
        assert!(styles.contains(expected), "missing style {expected:?}");
    }
}

#[test]
/// Keep literal source mode free of Markdown semantic runs and parser-owned metadata.
fn source_mode_does_not_create_preview_runs() {
    // Rebuild recognizable Markdown in source mode so accidental parsing would be observable.
    let source = "# **heading**";
    let mut presentation = MarkdownViewportPresentation::default();
    assert_eq!(
        presentation.set_mode(MarkdownPresentationMode::Source),
        None
    );
    presentation
        .rebuild(3, 0..source.encode_utf16().count(), source)
        .expect("record source-mode viewport");

    // Queries must cover the source window with empty styles and retain no semantic runs.
    assert!(presentation.runs.is_empty());
    assert!(presentation.tables().is_empty());
    assert!(!presentation.needs_rebuild(3, 1));
    assert_eq!(
        presentation.cached_utf16_window(),
        Some(0..source.encode_utf16().count())
    );
}

#[test]
/// Keep native popup indices stable and reject unsupported future mode values.
fn popup_mode_mapping_is_bidirectional_and_rejects_future_values() {
    // Check both directions for each supported mode and explicitly reject an unknown index.
    assert_eq!(MarkdownPresentationMode::InlinePreview.popup_index(), 0);
    assert_eq!(MarkdownPresentationMode::Source.popup_index(), 1);
    assert_eq!(
        MarkdownPresentationMode::from_popup_index(0),
        Some(MarkdownPresentationMode::InlinePreview)
    );
    assert_eq!(
        MarkdownPresentationMode::from_popup_index(1),
        Some(MarkdownPresentationMode::Source)
    );
    assert_eq!(MarkdownPresentationMode::from_popup_index(2), None);
    assert_eq!(MarkdownPresentationMode::from_popup_index(usize::MAX), None);
}

#[test]
/// Preserve bounded native invalidation coverage without retaining parsed source-mode metadata.
fn source_window_is_recorded_once_and_invalidated_by_generation_change() {
    // Record one literal window and verify a same-generation position is already covered.
    let mut presentation = MarkdownViewportPresentation::default();
    presentation.set_mode(MarkdownPresentationMode::Source);
    presentation
        .record_source_window(11, 20..80)
        .expect("record bounded source window");
    assert!(!presentation.needs_rebuild(11, 20));
    assert!(!presentation.needs_rebuild(11, 79));
    assert!(presentation.needs_rebuild(11, 80));
    assert_eq!(presentation.cached_utf16_window(), Some(20..80));

    // Generation invalidation must return only the previous window and make cached queries unavailable.
    let invalidated = presentation.invalidate_document_generation();
    assert_eq!(invalidated, Some(20..80));
    assert!(presentation.needs_rebuild(11, 20));
    assert_eq!(presentation.cached_utf16_window(), None);
}

#[test]
/// Reveal source syntax for the selected inline structure without exposing unrelated delimiters.
fn only_the_active_inline_structure_reveals_its_markers() {
    // Place two independently owned inline structures in one viewport and select only the first.
    let source = "**first** and *second*";
    let active = source.find("first").expect("first structure") + 2;
    let presentation = rebuilt_presentation(source, 0, active..active);
    let first_marker = presentation.query(0).expect("first strong marker");
    let second_start = source.find("*second*").expect("second structure");
    let second_marker = presentation
        .query(second_start)
        .expect("second emphasis marker");

    // The first marker remains visible while the second marker keeps its hidden role.
    assert!(
        first_marker
            .styles
            .contains(MarkdownPresentationStyles::MARKER)
    );
    assert!(
        !first_marker
            .styles
            .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
    );
    assert!(
        second_marker
            .styles
            .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
    );
}

#[test]
/// Project parser byte ranges through Unicode into correctly offset absolute native coordinates.
fn unicode_byte_offsets_become_absolute_utf16_ranges_inside_the_window() {
    // Combine a surrogate-pair prefix, CJK strong text, and a combining suffix at a nonzero viewport origin.
    let source = "😀 **中文** e\u{301}";
    let absolute_start = 41;
    let presentation = rebuilt_presentation(source, absolute_start, 0..0);
    let strong_marker_local_utf16 = "😀 ".encode_utf16().count();
    let marker = presentation
        .query(absolute_start + strong_marker_local_utf16)
        .expect("strong marker after surrogate pair");

    // Marker and text ranges must match independently counted UTF-16 positions with no interior encoding boundaries.
    assert_eq!(
        marker.utf16_range,
        absolute_start + strong_marker_local_utf16..absolute_start + strong_marker_local_utf16 + 2
    );
    assert!(
        marker
            .styles
            .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
    );
    assert!(presentation.runs.iter().all(|run| {
        run.utf16_range.start >= absolute_start
            && run.utf16_range.end <= absolute_start + source.encode_utf16().count()
    }));
}

#[test]
/// Keep cache invalidation exact across selection, document generation, and presentation mode transitions.
fn generation_selection_and_mode_changes_invalidate_only_the_bounded_window() {
    // Build a small semantic window at a nonzero document origin.
    let source = "# heading\n\nbody";
    let mut presentation = rebuilt_presentation(source, 100, 110..110);
    assert!(!presentation.needs_rebuild(7, 110));
    assert!(presentation.needs_rebuild(8, 110));

    // A changed selection invalidates only that previously cached window.
    let selection_invalidation = presentation
        .set_active_selection(101..101)
        .expect("selection changes preview context");
    assert_eq!(
        selection_invalidation,
        100..100 + source.encode_utf16().count()
    );
    assert!(presentation.needs_rebuild(7, 110));

    // Rebuild, switch mode, and change generation while checking the same bounded invalidation contract.
    presentation
        .rebuild(7, 100..100 + source.encode_utf16().count(), source)
        .expect("rebuild after selection change");
    let mode_invalidation = presentation
        .set_mode(MarkdownPresentationMode::Source)
        .expect("mode changes preview attributes");
    assert_eq!(mode_invalidation, selection_invalidation);
}

#[test]
/// Keep setext heading style separate from its hidden source underline markers.
fn setext_markers_are_hidden_without_underlining_the_heading_text() {
    // Use both supported setext heading levels under an inactive selection.
    let source = "Primary\n=======\n\nSecondary\n---------\n";
    let presentation = rebuilt_presentation(source, 0, source.len()..source.len());
    let first_marker = source.find("=======").expect("setext marker");
    let title = presentation.query(1).expect("heading title");
    let marker = presentation.query(first_marker).expect("setext marker run");

    // Title text must retain heading roles while delimiter rows remain hidden source syntax.
    assert!(title.styles.contains(MarkdownPresentationStyles::HEADING_1));
    assert!(!title.styles.contains(MarkdownPresentationStyles::UNDERLINE));
    assert!(
        marker
            .styles
            .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
    );
}

#[test]
/// Restrict underline styling to the exact supported underline-tag allowlist.
fn headings_and_links_are_not_underlined_but_explicit_u_is() {
    // Place a heading, ordinary link, and exact underline pair in one source envelope.
    let source = "# Heading\n\n[link](https://example.test)\n\n<u>underlined</u>";
    let presentation = rebuilt_presentation(source, 0, source.encode_utf16().count()..0);

    // A heading must receive heading styling without inheriting underline.
    let heading = presentation.query(2).expect("heading text run");
    assert!(
        heading
            .styles
            .contains(MarkdownPresentationStyles::HEADING_1)
    );
    assert!(
        !heading
            .styles
            .contains(MarkdownPresentationStyles::UNDERLINE)
    );

    // A link must receive link styling without automatically inheriting underline.
    let link_start = source.find("[link]").expect("link start");
    let link = presentation.query(link_start + 1).expect("link text run");
    assert!(link.styles.contains(MarkdownPresentationStyles::LINK));
    assert!(!link.styles.contains(MarkdownPresentationStyles::UNDERLINE));

    // The explicit paired underline content must receive underline while its tags remain source markers.
    let underline_start = source.find("underlined").expect("underline text");
    let underline = presentation
        .query(underline_start)
        .expect("explicit underline run");
    assert!(
        underline
            .styles
            .contains(MarkdownPresentationStyles::UNDERLINE)
    );
}

#[test]
/// Change marker hiding from ownership and selection without changing source coordinates.
fn marker_visibility_changes_only_for_the_active_selection() {
    // Build inactive heading and strong structures in one window.
    let source = "# Heading\n\n**bold**";
    let heading_marker = source.find('#').expect("heading marker");
    let active = source.find("bold").expect("bold text") + 1;
    let presentation = rebuilt_presentation(source, 0, active..active);
    let marker = presentation
        .query(heading_marker)
        .expect("heading marker run");
    assert!(marker.styles.contains(MarkdownPresentationStyles::MARKER));
    assert!(
        marker
            .styles
            .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
    );

    // Activate the heading marker and verify its syntax is revealed while unrelated strong syntax remains hidden.
    let active_heading = rebuilt_presentation(source, 0, heading_marker..heading_marker + 1);
    let visible_marker = active_heading
        .query(heading_marker)
        .expect("visible marker run");
    assert!(
        visible_marker
            .styles
            .contains(MarkdownPresentationStyles::MARKER)
    );
    assert!(
        !visible_marker
            .styles
            .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
    );
}

#[test]
/// Preserve editable source ownership for all three CommonMark thematic-break marker families.
fn commonmark_horizontal_rules_collapse_until_their_source_is_active() {
    // Each supported thematic-break spelling must hide while inactive and reveal when the caret activates it.
    for source in ["---\n", "***\n", "___\n"] {
        // Test every supported marker family under inactive and active selections with the same ownership assertions.
        let inactive_selection = source.encode_utf16().count() + 1;
        let inactive = rebuilt_presentation(source, 0, inactive_selection..inactive_selection);
        let marker = inactive.query(0).expect("horizontal rule marker");
        assert!(
            marker
                .styles
                .contains(MarkdownPresentationStyles::HORIZONTAL_RULE)
        );
        assert!(
            marker
                .styles
                .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
        );

        let active = rebuilt_presentation(source, 0, 1..1);
        let visible = active.query(1).expect("active horizontal rule source");
        assert!(
            visible
                .styles
                .contains(MarkdownPresentationStyles::HORIZONTAL_RULE)
        );
        assert!(
            !visible
                .styles
                .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
        );
    }
}

#[test]
/// Preserve table roles, alignments, source extents, and flat cell ownership from authoritative parser events.
fn gfm_table_uses_parser_cells_and_preserves_six_column_semantics() {
    // Use a six-column Unicode table with varied alignment and nested inline content.
    let source = [
        "| 任务编号 (ID) | 模块名称 (Module) | 运行状态 (Status) | 优先级 (Priority) | 响应时间 (Latency) | 详细说明 (Notes) |",
        "| :--- | :--- | :---: | :---: | ---: | :--- |",
        "| `#101` | **用户鉴权中心** `Auth` | 🟢 正常 | `P0` | `12 ms` | 支持 OAuth 2.0 与 JWT 校验 |",
        "| `#102` | **数据同步通道** `Sync` | 🟡 排队中 | `P1` | `145 ms` | 正在等待跨区域节点就绪 |",
        "| `#103` | **推送网关** `Push` | 🔴 异常 | `P0` | `--` | 触发下游接口超时熔断 |",
        "| `#104` | **报表导出** `Export` | ⚪ 已完成 | `P2` | `3,420 ms` | 生成文件大小约 *12.5 MB* |",
    ]
    .join("\n");
    let inactive_selection = source.encode_utf16().count() + 1;
    let presentation = rebuilt_presentation(&source, 0, inactive_selection..inactive_selection);

    // Left-aligned header content must carry header, table, and left-alignment roles.
    let header_offset =
        utf16_offset_at_byte(&source, source.find("任务编号").expect("left header"));
    let header = presentation
        .query(header_offset)
        .expect("left-aligned table header");
    assert!(
        header
            .styles
            .contains(MarkdownPresentationStyles::TABLE_HEADER)
    );
    assert!(
        header
            .styles
            .contains(MarkdownPresentationStyles::TABLE_ALIGN_LEFT)
    );

    // Centered header content must preserve its explicit alignment role.
    let center_offset =
        utf16_offset_at_byte(&source, source.find("运行状态").expect("center header"));
    let center = presentation
        .query(center_offset)
        .expect("center-aligned table header");
    assert!(
        center
            .styles
            .contains(MarkdownPresentationStyles::TABLE_ALIGN_CENTER)
    );

    // Right-aligned header content must preserve its explicit alignment role.
    let right_offset =
        utf16_offset_at_byte(&source, source.find("响应时间").expect("right header"));
    let right = presentation
        .query(right_offset)
        .expect("right-aligned table header");
    assert!(
        right
            .styles
            .contains(MarkdownPresentationStyles::TABLE_ALIGN_RIGHT)
    );

    // Structural table pipes must remain hidden boundary markers rather than cell text.
    let boundary = presentation.query(0).expect("leading table boundary");
    assert!(
        boundary
            .styles
            .contains(MarkdownPresentationStyles::TABLE_BOUNDARY)
    );
    assert!(
        boundary
            .styles
            .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
    );

    // The delimiter row retains table delimiter semantics and hidden source syntax.
    let delimiter_offset =
        utf16_offset_at_byte(&source, source.find(":---").expect("table delimiter"));
    let delimiter = presentation
        .query(delimiter_offset)
        .expect("collapsed table delimiter");
    assert!(
        delimiter
            .styles
            .contains(MarkdownPresentationStyles::TABLE_DELIMITER)
    );
    assert!(
        delimiter
            .styles
            .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
    );

    // Nested strong and other inline table content must retain both table and inline roles.
    let strong_offset = utf16_offset_at_byte(
        &source,
        source.find("用户鉴权中心").expect("strong table content"),
    );
    assert!(
        presentation
            .query(strong_offset)
            .expect("strong table content style")
            .styles
            .contains(MarkdownPresentationStyles::STRONG)
    );
    let code_offset = utf16_offset_at_byte(
        &source,
        source.find("#101").expect("inline code table content"),
    );
    assert!(
        presentation
            .query(code_offset)
            .expect("inline code table content style")
            .styles
            .contains(MarkdownPresentationStyles::INLINE_CODE)
    );
    let emphasis_offset = utf16_offset_at_byte(
        &source,
        source.find("12.5 MB").expect("emphasis table content"),
    );
    assert!(
        presentation
            .query(emphasis_offset)
            .expect("emphasis table content style")
            .styles
            .contains(MarkdownPresentationStyles::EMPHASIS)
    );

    // Inspect the immutable prototype to verify exact row/cell counts, alignment, flat indices, and source-coordinate coverage.
    let tables = presentation.tables();
    assert_eq!(tables.len(), 1);
    let table = &tables[0];
    assert_eq!(table.rows.len(), 5);
    assert_eq!(table.cells.len(), 30);
    assert!(table.rows[0].is_header);
    for (row_index, row) in table.rows.iter().enumerate() {
        // Rows must index six consecutive cells in the single flat vector without overlap or omission.
        assert_eq!(row.cell_index_range, row_index * 6..row_index * 6 + 6);
        assert_eq!(row.is_header, row_index == 0);
    }
    let expected_alignments = [
        MarkdownTableAlignment::Left,
        MarkdownTableAlignment::Left,
        MarkdownTableAlignment::Center,
        MarkdownTableAlignment::Center,
        MarkdownTableAlignment::Right,
        MarkdownTableAlignment::Left,
    ];
    for (cell_index, cell) in table.cells.iter().enumerate() {
        // Every cell keeps its original column and the independently expected alignment for that column.
        assert_eq!(cell.column_index, cell_index % 6);
        assert_eq!(cell.alignment, expected_alignments[cell_index % 6]);
    }
    let row_sources = [
        source.lines().next().expect("header row"),
        source.lines().nth(2).expect("first body row"),
        source.lines().nth(3).expect("second body row"),
        source.lines().nth(4).expect("third body row"),
        source.lines().nth(5).expect("fourth body row"),
    ];
    for (row, row_source) in table.rows.iter().zip(row_sources) {
        // Each row's absolute UTF-16 extent must match its exact source line.
        let byte_start = source.find(row_source).expect("table row source range");
        let utf16_start = utf16_offset_at_byte(&source, byte_start);
        assert_eq!(
            row.utf16_range,
            utf16_start..utf16_start + row_source.encode_utf16().count()
        );
    }
    let emoji_offset = utf16_offset_at_byte(
        &source,
        source.find('🟢').expect("supplementary table scalar"),
    );
    assert!(table.cells[8].utf16_range.contains(&emoji_offset));
    assert!(table.cells[8].utf16_range.contains(&(emoji_offset + 1)));
    assert_eq!(table.header_utf16_range.start, 0);
    assert_eq!(
        table.delimiter_utf16_range.start,
        source.lines().next().unwrap().encode_utf16().count() + 1
    );
    assert!(table.utf16_range.end <= source.encode_utf16().count());
}

#[test]
/// Prevent escaped and inline-code pipes from becoming fabricated table boundaries.
fn gfm_escaped_pipes_remain_inside_authoritative_parser_cells() {
    // Use escaped pipe text inside both inline code and ordinary cells.
    let source = "| A | B |\n| --- | --- |\n| `a\\|b` | c\\|d |";
    let presentation = rebuilt_presentation(source, 0, source.encode_utf16().count()..0);
    let table = presentation.tables().first().expect("GFM table prototype");

    // Prototype counts must match parser cells and those pipe positions must retain literal cell styles.
    assert_eq!(table.rows.len(), 2);
    assert_eq!(table.cells.len(), 4);
    assert_eq!(&source[table.cells[2].utf16_range.clone()], "`a\\|b`");
    assert_eq!(&source[table.cells[3].utf16_range.clone()], "c\\|d");
    for escaped_pipe in [
        source.find("\\|").unwrap() + 1,
        source.rfind("\\|").unwrap() + 1,
    ] {
        // Both escaped pipes are parser-owned cell content and must never become hidden table-boundary decorations.
        let styles = presentation
            .query(escaped_pipe)
            .expect("escaped pipe presentation")
            .styles;
        assert!(!styles.contains(MarkdownPresentationStyles::TABLE_BOUNDARY));
    }
}

#[test]
/// Discard stale table semantics when an oversized block requires bounded literal fallback.
fn source_fallback_records_a_plain_window_without_parsing_a_partial_table() {
    // First build a valid table so stale metadata would be observable.
    let source = "| A | B |\n| --- | --- |\n| 1 | 2 |\n";
    let mut presentation = rebuilt_presentation(source, 100, 0..0);
    assert!(!presentation.tables().is_empty());

    // Record a different source-fallback window without supplying partial table text to the parser.
    presentation
        .record_source_fallback_window(8, 120..140)
        .expect("record source fallback window");

    // No stale tables or style runs may remain, and only the fallback interval is queryable.
    assert!(presentation.tables().is_empty());
    assert!(presentation.runs.is_empty());
    assert!(!presentation.needs_rebuild(8, 125));
    let query = presentation.query(125).expect("plain fallback query");
    assert!(query.styles.is_empty());
    assert_eq!(query.utf16_range, 120..140);
}

#[test]
/// Keep YAML metadata interpretation tied to the actual document beginning.
fn yaml_metadata_is_enabled_only_at_the_real_document_start() {
    // At absolute offset zero, metadata contents remain literal unstyled source rather than thematic breaks.
    let metadata = "---\ntitle: Omni\n---\n\n# Heading\n";
    let presentation = rebuilt_presentation(metadata, 0, metadata.len()..metadata.len());
    let first = presentation.query(0).expect("metadata source run");
    assert!(
        !first
            .styles
            .contains(MarkdownPresentationStyles::HORIZONTAL_RULE)
    );

    // The same delimiter spelling at a nonzero origin must follow ordinary Markdown rule semantics.
    let horizontal_rule = "---\n\nbody\n";
    let absolute_start = 100;
    let presentation = rebuilt_presentation(
        horizontal_rule,
        absolute_start,
        absolute_start + horizontal_rule.len()..absolute_start + horizontal_rule.len(),
    );
    let rule = presentation
        .query(absolute_start)
        .expect("non-leading horizontal rule");
    assert!(
        rule.styles
            .contains(MarkdownPresentationStyles::HORIZONTAL_RULE)
    );
}

#[test]
/// Keep native table placeholders independent of selection-sensitive inline marker visibility.
fn inline_table_source_markers_remain_hidden_for_active_selection() {
    // Build a compact table with the selection inside it and inspect its delimiter semantics.
    let source = "Name | Value\n:--- | ---:\nOmni | Fast";
    let delimiter_offset = source.find(":---").expect("table delimiter");
    let presentation = rebuilt_presentation(source, 0, delimiter_offset + 1..delimiter_offset + 1);
    let delimiter = presentation
        .query(delimiter_offset)
        .expect("active delimiter source");
    assert!(
        delimiter
            .styles
            .contains(MarkdownPresentationStyles::TABLE_DELIMITER)
    );
    assert!(
        delimiter
            .styles
            .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
    );

    // Header pipes remain hidden table boundaries even while the native table is being edited.
    let header_boundary = presentation
        .query(source.find('|').expect("header boundary"))
        .expect("header boundary run");
    assert!(
        header_boundary
            .styles
            .contains(MarkdownPresentationStyles::TABLE_BOUNDARY)
    );
    assert!(
        header_boundary
            .styles
            .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
    );
}

#[test]
/// Keep table prototypes and semantic runs within the supplied fixed-size UTF-16 window.
fn table_presentation_remains_inside_a_4096_utf16_window() {
    // Embed a Unicode table in source padded to exactly 4096 UTF-16 units.
    let table = "| 名称 | 值 |\n| :--- | ---: |\n| Omni | 高性能 |\n";
    let suffix_length = 4_096 - table.encode_utf16().count();
    let source = format!("{table}{}", "x".repeat(suffix_length));
    let presentation = rebuilt_presentation(&source, 500, 4_597..4_597);

    // Every published semantic interval must remain within the declared absolute window.
    assert_eq!(source.encode_utf16().count(), 4_096);
    assert!(
        presentation
            .runs
            .iter()
            .all(|run| { run.utf16_range.start >= 500 && run.utf16_range.end <= 4_596 })
    );
}
