//! Models bounded visible-table geometry, cell identity and keyboard navigation.

use super::*;

/// Selects one direction in the canonical structural history, independent of AppKit undo storage.
#[derive(Clone, Copy)]
pub(super) enum MarkdownHistoryDirection {
    Undo,
    Redo,
}

/// Identifies a semantic cell after reparsing; no cached byte offset survives a preceding cell edit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct NativeMarkdownTableCellCoordinate {
    pub(super) table_utf16_start: usize,
    pub(super) row_index: usize,
    pub(super) column_index: usize,
}

/// Owns only one visible cell's derived geometry and immutable attributed display text.
#[derive(Clone)]
pub(super) struct NativeMarkdownVisibleTableCellLayout {
    pub(super) coordinate: NativeMarkdownTableCellCoordinate,
    pub(super) source_utf16_range: std::ops::Range<usize>,
    pub(super) frame: NSRect,
    pub(super) text_frame: NSRect,
    pub(super) is_header: bool,
    pub(super) alignment: MarkdownTableAlignment,
    pub(super) attributed_text: Retained<NSAttributedString>,
}

/// Supplies background/header geometry for a row already selected by bounded viewport culling.
#[derive(Clone)]
pub(super) struct NativeMarkdownVisibleTableRowLayout {
    pub(super) row_index: usize,
    pub(super) frame: NSRect,
    pub(super) is_header: bool,
}

/// Groups visible row/cell geometry under one table source identity and column coordinate system.
#[derive(Clone)]
pub(super) struct NativeMarkdownVisibleTableLayout {
    pub(super) table_utf16_start: usize,
    pub(super) frame: NSRect,
    pub(super) rows: Vec<NativeMarkdownVisibleTableRowLayout>,
    pub(super) column_offsets: Vec<f64>,
    pub(super) cells: Vec<NativeMarkdownVisibleTableCellLayout>,
}
/// Stable identity of one table layout request.
///
/// TextKit may reenter drawRect: repeatedly. Unchanged generation, viewport and width
/// perform at most one layout, including empty/failure results, to avoid idle wakeups.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct NativeMarkdownTableLayoutRequestKey {
    pub(super) document_generation: u64,
    pub(super) document_length: usize,
    pub(super) presentation_mode: MarkdownPresentationMode,
    pub(super) visible_origin_x: i64,
    pub(super) visible_origin_y: i64,
    pub(super) visible_width: i64,
    pub(super) visible_height: i64,
    pub(super) text_view_width: i64,
}

/// Retains derived presentation for one exact generation/viewport request until invalidation or close.
#[derive(Clone)]
pub(super) struct NativeMarkdownVisibleTableLayoutCache {
    pub(super) document_generation: u64,
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) utf16_window: std::ops::Range<usize>,
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) viewport_utf16_window: std::ops::Range<usize>,
    pub(super) text_view_width: f64,
    pub(super) tables: Vec<NativeMarkdownVisibleTableLayout>,
    pub(super) request_key: NativeMarkdownTableLayoutRequestKey,
}

/// Stores the active single-cell draft and source range until explicit commit or cancellation.
#[derive(Clone)]
pub(super) struct NativeMarkdownTableCellEditorState {
    pub(super) coordinate: NativeMarkdownTableCellCoordinate,
    pub(super) source_utf16_range: std::ops::Range<usize>,
    pub(super) frame: NSRect,
    pub(super) original_source: String,
}

/// Encodes spreadsheet-style movement without coupling semantic coordinates to native controls.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NativeMarkdownTableNavigation {
    Next,
    Previous,
    NextRow,
}

/// Normalizes viewport coordinates into a stable, finite cache-key metric.
pub(super) fn native_markdown_layout_metric(value: f64) -> i64 {
    if value.is_finite() {
        // Quantize AppKit's point coordinates to sixteenths. This filters tiny floating-point
        // layout jitter while preserving meaningful resize and scroll invalidations.
        (value * 16.0).round() as i64
    } else {
        // Nonfinite transient native geometry must not create unstable cache identities.
        0
    }
}

/// Finds visible rows by ordered UTF-16 intervals and adds at most one overscan row per edge.
pub(super) fn native_markdown_visible_table_row_range(
    rows: &[omni_markdown_core::MarkdownTableRowPrototype],
    viewport_utf16_window: &std::ops::Range<usize>,
) -> std::ops::Range<usize> {
    if rows.is_empty() {
        // Empty semantic tables cannot contribute visible geometry or an overscan row.
        return 0..0;
    }

    // Two partition searches preserve logarithmic row selection even in very large tables.
    let viewport_end = viewport_utf16_window
        .end
        .max(viewport_utf16_window.start.saturating_add(1));
    let first_visible =
        rows.partition_point(|row| row.utf16_range.end <= viewport_utf16_window.start);
    let visible_end = rows.partition_point(|row| row.utf16_range.start < viewport_end);
    if first_visible < visible_end {
        // Include visible rows and one adjacent row so small scroll changes do not expose a blank edge.
        return first_visible.saturating_sub(1)..visible_end.saturating_add(1).min(rows.len());
    }

    // A gap or caret-only viewport uses the closest semantic row with the same fixed overscan budget.
    let nearest = first_visible.min(rows.len().saturating_sub(1));
    nearest.saturating_sub(1)..nearest.saturating_add(2).min(rows.len())
}

/// Resolves Tab, Shift-Tab or Enter against current parsed cells, preserving ragged-table boundaries.
pub(super) fn native_markdown_table_navigation_target(
    table: &MarkdownTablePrototype,
    current: NativeMarkdownTableCellCoordinate,
    navigation: NativeMarkdownTableNavigation,
) -> Option<NativeMarkdownTableCellCoordinate> {
    if table.utf16_range.start != current.table_utf16_start {
        // A coordinate belonging to an older/different table cannot move this table's editing focus.
        return None;
    }
    // Missing rows/cells are stale coordinates; return no target rather than inventing a new cell.
    let row = table.rows.get(current.row_index)?;
    let current_cell_index = row.cell_index_range.clone().find(|cell_index| {
        // Resolve the semantic column inside this row instead of assuming a rectangular flat index.
        table
            .cells
            .get(*cell_index)
            .is_some_and(|cell| cell.column_index == current.column_index)
    })?;

    let target_cell_index = match navigation {
        NativeMarkdownTableNavigation::Next => {
            // Tab advances horizontally, then wraps only to an existing next row.
            if current_cell_index + 1 < row.cell_index_range.end {
                // The next cell remains within the current row's authoritative parsed interval.
                current_cell_index + 1
            } else {
                // At the row edge, the next parsed row may be absent or empty.
                let next_row = table.rows.get(current.row_index.saturating_add(1))?;
                next_row.cell_index_range.clone().next()?
            }
        }
        NativeMarkdownTableNavigation::Previous => {
            // Shift-Tab reverses horizontal traversal and wraps to the preceding row's last cell.
            if current_cell_index > row.cell_index_range.start {
                // A preceding cell exists in the current row.
                current_cell_index - 1
            } else {
                // Checked subtraction makes the first cell of the first row a terminal boundary.
                let previous_row = table.rows.get(current.row_index.checked_sub(1)?)?;
                previous_row.cell_index_range.clone().next_back()?
            }
        }
        NativeMarkdownTableNavigation::NextRow => {
            // Enter preserves the semantic column; a ragged next row can legitimately have no target.
            let next_row = table.rows.get(current.row_index.saturating_add(1))?;
            next_row.cell_index_range.clone().find(|cell_index| {
                // Search only existing next-row cells rather than creating implicit padding columns.
                table
                    .cells
                    .get(*cell_index)
                    .is_some_and(|cell| cell.column_index == current.column_index)
            })?
        }
    };
    // Reconstruct the destination from authoritative parsed data after resolving its flat cell index.
    let target = table.cells.get(target_cell_index)?;
    let row_index = table
        .rows
        .iter()
        .position(|row| row.cell_index_range.contains(&target_cell_index))?;
    Some(NativeMarkdownTableCellCoordinate {
        table_utf16_start: table.utf16_range.start,
        row_index,
        column_index: target.column_index,
    })
}

/// Fits natural column widths to the viewport while enforcing useful bounded widths where space permits.
pub(super) fn native_markdown_table_column_widths(
    natural_widths: &[f64],
    available_width: f64,
) -> Vec<f64> {
    if natural_widths.is_empty() || available_width <= 0.0 {
        // A zero-column or collapsed viewport has no meaningful horizontal layout to allocate.
        return Vec::new();
    }
    // Relax the normal minimum for narrow viewports before clamping each measured natural width.
    let column_count = natural_widths.len() as f64;
    let dynamic_minimum =
        NATIVE_MARKDOWN_TABLE_MINIMUM_COLUMN_WIDTH.min((available_width / column_count).max(42.0));
    let mut widths: Vec<f64> = natural_widths
        .iter()
        .map(|width| width.clamp(dynamic_minimum, NATIVE_MARKDOWN_TABLE_MAXIMUM_COLUMN_WIDTH))
        .collect();
    let mut total: f64 = widths.iter().sum();
    if total < available_width {
        // Distribute spare width fairly among columns that have not reached the maximum.
        let mut remaining = available_width - total;
        // Each iteration consumes spare width or stops when every column is capped; half a point
        // prevents floating-point residue from turning viewport layout into unbounded repeated work.
        while remaining > 0.5 {
            let expandable = widths
                .iter()
                .filter(|width| **width < NATIVE_MARKDOWN_TABLE_MAXIMUM_COLUMN_WIDTH)
                .count();
            if expandable == 0 {
                // Every column reached the cap; leaving trailing space is preferable to oversized cells.
                break;
            }
            let share = remaining / expandable as f64;
            let mut consumed = 0.0;
            // Apply the same share with individual caps, then redistribute any unconsumed remainder.
            for width in &mut widths {
                let growth = share.min(NATIVE_MARKDOWN_TABLE_MAXIMUM_COLUMN_WIDTH - *width);
                *width += growth;
                consumed += growth;
            }
            if consumed <= 0.5 {
                // No meaningful expansion remains, so another pass would only add numerical noise.
                break;
            }
            remaining -= consumed;
        }
        return widths;
    }
    if total > available_width {
        // Remove overflow proportionally to each column's capacity above the dynamic minimum.
        let shrinkable: f64 = widths
            .iter()
            .map(|width| (*width - dynamic_minimum).max(0.0))
            .sum();
        let overflow = total - available_width;
        if shrinkable > 0.0 {
            // At least one column can shrink without first crossing the normal dynamic minimum.
            for width in &mut widths {
                // A column contributes only its own nonnegative share of the aggregate shrink capacity.
                let capacity = (*width - dynamic_minimum).max(0.0);
                *width -= overflow * (capacity / shrinkable);
            }
        }
        total = widths.iter().sum();
        if total > available_width + 0.5 {
            // An exceptionally narrow viewport takes equal widths instead of overflowing horizontally.
            widths.fill(available_width / column_count);
        }
    }
    widths
}

/// Keeps committed cell text on one Markdown row and escapes structural pipes exactly once.
pub(super) fn native_markdown_sanitize_table_cell_input(source: &str) -> String {
    let mut sanitized = String::with_capacity(source.len());
    let mut consecutive_backslashes = 0_usize;
    // Count immediately preceding backslashes while streaming scalars, with no second normalized copy.
    for scalar in source.chars() {
        match scalar {
            '\r' | '\n' => {
                // Newlines must not split the enclosing table row; collapse consecutive separators.
                if !sanitized.ends_with(' ') {
                    // Preserve a word boundary without multiplying spaces for CRLF or repeated newlines.
                    sanitized.push(' ');
                }
                consecutive_backslashes = 0;
            }
            '|' if consecutive_backslashes.is_multiple_of(2) => {
                // An even escape run leaves this pipe structural, so add exactly one Markdown escape.
                sanitized.push('\\');
                sanitized.push('|');
                consecutive_backslashes = 0;
            }
            '\\' => {
                // Preserve literal backslashes and remember parity for the immediately following pipe.
                sanitized.push('\\');
                consecutive_backslashes = consecutive_backslashes.saturating_add(1);
            }
            _ => {
                // Ordinary Unicode text and already escaped pipes preserve their original scalar bytes.
                sanitized.push(scalar);
                consecutive_backslashes = 0;
            }
        }
    }
    sanitized.trim().to_owned()
}

/// Emits one small three-column Markdown table, separating surrounding blocks only when necessary.
pub(super) fn native_markdown_default_table_source(
    needs_prefix: bool,
    needs_suffix: bool,
) -> String {
    const TABLE_SOURCE: &str =
        "| 列 1 | 列 2 | 列 3 |\n| :--- | :---: | ---: |\n|  |  |  |\n|  |  |  |";
    let mut markdown = String::with_capacity(
        TABLE_SOURCE.len() + usize::from(needs_prefix) * 2 + usize::from(needs_suffix) * 2,
    );
    if needs_prefix {
        // Existing nonempty text before the insertion needs a blank block boundary.
        markdown.push_str("\n\n");
    }
    markdown.push_str(TABLE_SOURCE);
    if needs_suffix {
        // Existing text after the insertion must not become part of the table's final row.
        markdown.push_str("\n\n");
    }
    markdown
}

#[cfg(test)]
mod native_markdown_table_tests {
    use super::{
        MarkdownPresentationMode, MarkdownPresentationStyles, MarkdownTableAlignment,
        MarkdownTablePrototype, MarkdownViewportPresentation, NativeMarkdownTableCellCoordinate,
        NativeMarkdownTableNavigation, native_markdown_default_table_source,
        native_markdown_sanitize_table_cell_input,
        native_markdown_table_cell_display_text_from_source, native_markdown_table_column_widths,
        native_markdown_table_navigation_target, native_markdown_visible_table_row_range,
    };
    use omni_markdown_core::{MarkdownTableCellPrototype, MarkdownTableRowPrototype};

    /// Build an ordered rectangular semantic fixture without parsing or native object allocation.
    pub(super) fn uniform_table_prototype(
        row_count: usize,
        column_count: usize,
    ) -> MarkdownTablePrototype {
        let mut rows = Vec::with_capacity(row_count);
        let mut cells = Vec::with_capacity(row_count.saturating_mul(column_count));
        // Build a deterministic rectangular semantic fixture; every row owns a disjoint cell interval.
        for row_index in 0..row_count {
            let row_start = row_index * 10;
            let cell_start = cells.len();
            // Scalar-sized synthetic cells keep the fixture focused on navigation and row culling.
            for column_index in 0..column_count {
                cells.push(MarkdownTableCellPrototype {
                    utf16_range: row_start + column_index..row_start + column_index + 1,
                    column_index,
                    alignment: MarkdownTableAlignment::Left,
                });
            }
            rows.push(MarkdownTableRowPrototype {
                utf16_range: row_start..row_start + 10,
                cell_index_range: cell_start..cells.len(),
                is_header: row_index == 0,
            });
        }
        MarkdownTablePrototype {
            utf16_range: 0..row_count * 10,
            header_utf16_range: 0..10,
            delimiter_utf16_range: 10..11,
            rows,
            cells,
        }
    }

    /// Derive one actual parser table while retaining only its owned finite prototype for assertions.
    pub(super) fn parsed_table(source: &str) -> MarkdownTablePrototype {
        let mut presentation = MarkdownViewportPresentation::default();
        presentation.set_mode(MarkdownPresentationMode::InlinePreview);
        presentation.set_active_selection(source.encode_utf16().count()..0);
        presentation
            .rebuild(1, 0..source.encode_utf16().count(), source)
            .expect("build fixture table semantics");
        presentation
            .tables()
            .first()
            .expect("the parsed fixture table")
            .clone()
    }

    #[test]
    /// Include exactly one adjacent row at either visible edge rather than materializing the table body.
    pub(super) fn visible_table_rows_include_exactly_one_overscan_row_per_side() {
        let table = uniform_table_prototype(10, 6);

        assert_eq!(
            native_markdown_visible_table_row_range(&table.rows, &(35..55)),
            2..7
        );
    }

    #[test]
    /// Keep cell materialization proportional to the viewport even when the semantic table has ten thousand rows.
    pub(super) fn large_table_materializes_only_visible_rows_and_bounded_overscan_cells() {
        let table = uniform_table_prototype(10_000, 6);
        let rows = native_markdown_visible_table_row_range(&table.rows, &(50_003..50_004));
        let visible_cell_count = rows
            .clone()
            .map(|row_index| table.rows[row_index].cell_index_range.len())
            .sum::<usize>();

        assert_eq!(rows, 4_999..5_002);
        assert_eq!(visible_cell_count, 18);
        assert!(visible_cell_count < table.cells.len());
    }

    #[test]
    /// Resolve horizontal wrapping, reverse wrapping and vertical column preservation at real row boundaries.
    pub(super) fn table_navigation_matches_spreadsheet_tab_backtab_and_enter_behavior() {
        let table = uniform_table_prototype(3, 6);
        let coordinate = |row_index, column_index| NativeMarkdownTableCellCoordinate {
            table_utf16_start: 0,
            row_index,
            column_index,
        };

        assert_eq!(
            native_markdown_table_navigation_target(
                &table,
                coordinate(0, 5),
                NativeMarkdownTableNavigation::Next,
            ),
            Some(coordinate(1, 0))
        );
        assert_eq!(
            native_markdown_table_navigation_target(
                &table,
                coordinate(1, 0),
                NativeMarkdownTableNavigation::Previous,
            ),
            Some(coordinate(0, 5))
        );
        assert_eq!(
            native_markdown_table_navigation_target(
                &table,
                coordinate(0, 3),
                NativeMarkdownTableNavigation::NextRow,
            ),
            Some(coordinate(1, 3))
        );
        assert_eq!(
            native_markdown_table_navigation_target(
                &table,
                coordinate(2, 5),
                NativeMarkdownTableNavigation::Next,
            ),
            None
        );
    }

    #[test]
    /// Preserve literal escaped pipes while preventing a cell draft from introducing another Markdown row/column.
    pub(super) fn table_cell_input_stays_single_line_and_escapes_structural_pipes() {
        assert_eq!(
            native_markdown_sanitize_table_cell_input(" a|b\nc\\|d "),
            "a\\|b c\\|d"
        );
    }

    #[test]
    /// Keep the finite source template parseable and insert blank block boundaries only where requested.
    pub(super) fn default_table_source_has_three_columns_and_optional_block_spacing() {
        let source = native_markdown_default_table_source(false, false);
        assert_eq!(source.lines().count(), 4);
        assert_eq!(parsed_table(&source).cells.len(), 9);
        assert!(native_markdown_default_table_source(true, false).starts_with("\n\n|"));
        assert!(native_markdown_default_table_source(false, true).ends_with("|\n\n"));
    }

    #[test]
    /// Translate hidden code delimiters and escaped pipes without shifting Unicode style ranges incorrectly.
    pub(super) fn table_cell_display_removes_commonmark_and_gfm_pipe_escapes() {
        let source = "`a\\|b` c\\|d 🟢";
        let display =
            native_markdown_table_cell_display_text_from_source(source, 0, |offset| match offset {
                // The two code delimiters disappear from derived cell display text.
                0 | 5 => MarkdownPresentationStyles::HIDDEN_MARKER,
                // Interior escaped pipe content keeps its inline-code style.
                1..=4 => MarkdownPresentationStyles::INLINE_CODE,
                // Remaining literal content inherits only table membership.
                _ => MarkdownPresentationStyles::TABLE,
            });

        assert_eq!(display.text, "a|b c|d 🟢");
        assert_eq!(display.text.encode_utf16().count(), 10);
        assert_eq!(display.style_spans[0].utf16_range, 0..3);
        assert!(
            display.style_spans[0]
                .styles
                .contains(MarkdownPresentationStyles::INLINE_CODE)
        );
    }

    #[test]
    /// Navigate by semantic row/column after an earlier cell changes every subsequent source offset.
    pub(super) fn semantic_cell_target_survives_cell_length_changes_without_index_caching() {
        let original = parsed_table("| A | B |\n| --- | --- |\n| x | y |");
        let edited = parsed_table("| A | B |\n| --- | --- |\n| much longer | y |");
        let current = NativeMarkdownTableCellCoordinate {
            table_utf16_start: edited.utf16_range.start,
            row_index: 1,
            column_index: 0,
        };

        assert!(edited.cells[3].utf16_range.start > original.cells[3].utf16_range.start);
        assert_eq!(
            native_markdown_table_navigation_target(
                &edited,
                current,
                NativeMarkdownTableNavigation::Next,
            ),
            Some(NativeMarkdownTableCellCoordinate {
                table_utf16_start: edited.utf16_range.start,
                row_index: 1,
                column_index: 1,
            })
        );
    }

    #[test]
    /// Fit normal viewports without oversized columns and distribute exceptionally narrow widths without overflow.
    pub(super) fn table_column_widths_fill_available_space_without_unbounded_columns() {
        let widths = native_markdown_table_column_widths(&[50.0, 200.0, 400.0], 600.0);
        assert!((widths.iter().sum::<f64>() - 600.0).abs() < 0.5);
        assert!(widths.iter().all(|width| *width > 0.0 && *width <= 280.0));

        let constrained = native_markdown_table_column_widths(&[200.0; 6], 240.0);
        assert_eq!(constrained, vec![40.0; 6]);
    }
}
