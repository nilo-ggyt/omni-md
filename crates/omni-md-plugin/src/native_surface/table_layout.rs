//! Prepares and paints bounded visible table/viewport geometry through public TextKit 2 layout.
//!
//! Queued weak work coalesces by generation and viewport; rendering reads finite immutable snapshots.
//! No RefCell borrow crosses native measurement or painting, and teardown cancels every queued request.

use super::*;

impl PieceBackedMarkdownTextView {
    /// Draws cached viewport table backgrounds, cells and grid lines without rebuilding semantics during painting.
    pub(super) fn draw_visible_markdown_tables(&self, dirty_rect: NSRect) {
        native_markdown_trace! {
            eprintln!(
                "[probe] draw tables begin manager={}",
                self.textLayoutManager().is_some()
            );
        }
        // Drawing reads prepared caches only. Parsing, layout and cell activation run in a
        // separate main-queue transaction to avoid reentering TextKit's own drawing stack.
        let Some(text_storage) = self.ivars().text_storage.load() else {
            // A late draw after teardown owns no source or palette and paints no overlay.
            return;
        };
        if text_storage.presentation_mode() != MarkdownPresentationMode::InlinePreview {
            // Source presentation uses only native text drawing; no table overlay may conceal raw Markdown.
            return;
        }
        let Some(request_key) = self.current_table_layout_request_key() else {
            // A detached document has no stable generation/viewport identity for this paint transaction.
            return;
        };
        // Clone this finite viewport projection before graphics/text drawing can reenter cache invalidation.
        let cache = self.ivars().visible_table_layout_cache.borrow().clone();
        let Some(cache) = cache
            .as_ref()
            .filter(|cache| cache.request_key == request_key)
        else {
            // An obsolete cache stays invisible until the coalesced native layout transaction prepares a replacement.
            return;
        };
        // Paint needs only Copy coordinate metadata, never a borrow of the editable field's draft state.
        let active_cell = self
            .ivars()
            .active_table_cell
            .borrow()
            .as_ref()
            .map(|active| active.coordinate);
        NSGraphicsContext::saveGraphicsState_class();
        // Paint only cached tables intersecting this native dirty rectangle.
        for table in &cache.tables {
            if !NSIntersectsRect(table.frame, dirty_rect) {
                // Distant cached geometry requires no drawing resources for this invalidation.
                continue;
            }
            // Row backgrounds remain separate from text so cell focus can paint above them.
            for row in &table.rows {
                if !NSIntersectsRect(row.frame, dirty_rect) {
                    // Native clipping excludes this row from the current paint pass.
                    continue;
                }
                let background = if row.is_header {
                    // Header tone distinguishes column labels without changing canonical styles.
                    &text_storage.ivars().palette.table_header_background
                } else {
                    // Body rows use the regular table background under their immutable display text.
                    &text_storage.ivars().palette.table_row_background
                };
                background.setFill();
                NSBezierPath::fillRect(row.frame);
            }
            // Draw only the already prepared visible-cell text; no source parse or native edit occurs.
            for cell in &table.cells {
                if !NSIntersectsRect(cell.frame, dirty_rect) {
                    // A cell outside the dirty clip cannot contribute visible pixels.
                    continue;
                }
                let is_active = active_cell == Some(cell.coordinate);
                if is_active {
                    // Native field editing gets a focused background while its committed source stays unchanged.
                    text_storage
                        .ivars()
                        .palette
                        .table_active_background
                        .setFill();
                    NSBezierPath::fillRect(cell.frame);
                    continue;
                }
                let options = NSStringDrawingOptions::UsesLineFragmentOrigin
                    | NSStringDrawingOptions::TruncatesLastVisibleLine;
                cell.attributed_text
                    .drawWithRect_options_context(cell.text_frame, options, None);
            }

            text_storage.ivars().palette.structure_border.setStroke();
            let grid = NSBezierPath::new();
            grid.setLineWidth(1.0);
            let minimum_y = table.frame.origin.y;
            let maximum_y = table.frame.origin.y + table.frame.size.height;
            // Shared prefix edges ensure adjacent cell borders use identical pixel coordinates.
            for offset in &table.column_offsets {
                let x = table.frame.origin.x + offset;
                grid.moveToPoint(NSPoint::new(x, minimum_y));
                grid.lineToPoint(NSPoint::new(x, maximum_y));
            }
            // Horizontal separators are emitted once per visible row rather than once per cell.
            for row in &table.rows {
                let y = row.frame.origin.y;
                grid.moveToPoint(NSPoint::new(table.frame.origin.x, y));
                grid.lineToPoint(NSPoint::new(
                    table.frame.origin.x + table.frame.size.width,
                    y,
                ));
            }
            if let Some(last_row) = table.rows.last() {
                // The final lower edge closes the visible grid after its last row.
                let y = last_row.frame.origin.y + last_row.frame.size.height;
                grid.moveToPoint(NSPoint::new(table.frame.origin.x, y));
                grid.lineToPoint(NSPoint::new(
                    table.frame.origin.x + table.frame.size.width,
                    y,
                ));
            }
            grid.stroke();
        }
        NSGraphicsContext::restoreGraphicsState_class();
        native_markdown_trace! {
            eprintln!(
                "[probe] draw tables end manager={}",
                self.textLayoutManager().is_some()
            );
        }
    }

    /// Snapshots document generation and finite viewport geometry to coalesce derived table layout.
    pub(super) fn current_table_layout_request_key(
        &self,
    ) -> Option<NativeMarkdownTableLayoutRequestKey> {
        let text_storage = self.ivars().text_storage.load()?;
        let (document_generation, document_length) = {
            let document = text_storage.document()?;
            let document = document.borrow();
            (document.generation(), document.utf16_len())
        };
        let visible_rect = self.visibleRect();
        let bounds = self.bounds();
        Some(NativeMarkdownTableLayoutRequestKey {
            document_generation,
            document_length,
            presentation_mode: text_storage.presentation_mode(),
            visible_origin_x: native_markdown_layout_metric(visible_rect.origin.x),
            visible_origin_y: native_markdown_layout_metric(visible_rect.origin.y),
            visible_width: native_markdown_layout_metric(visible_rect.size.width),
            visible_height: native_markdown_layout_metric(visible_rect.size.height),
            text_view_width: native_markdown_layout_metric(bounds.size.width),
        })
    }

    /// Queues at most one weakly owned foreground table-layout transaction for a changed viewport request.
    pub(super) fn schedule_visible_table_layout_update(&self) {
        // Native hierarchy removal can reenter resize/window callbacks while the old document
        // is still alive. Closing views must never acquire another layout transaction or cache.
        if self.ivars().textkit_graph_teardown.get() {
            // Teardown invalidates every future layout request; queued work must never reconnect the graph.
            return;
        }
        native_markdown_trace! {
            eprintln!(
                "[probe] schedule input_depth={} marked={:?} scheduled={} cache={}",
                self.ivars().textkit_input_depth.get(),
                self.ivars().marked_text_range.get(),
                self.ivars().table_layout_update_scheduled.get(),
                self.ivars().visible_table_layout_cache.borrow().is_some(),
            );
        }
        if self.ivars().textkit_input_depth.get() != 0
            || self
                .ivars()
                .marked_text_range
                .get()
                .is_some_and(|range| range.length != 0)
        {
            // Composition/native insertion will schedule again when its geometry becomes stable.
            return;
        }
        let Some(request_key) = self.current_table_layout_request_key() else {
            // A missing canonical owner cannot acquire new scheduled work.
            return;
        };
        if self.ivars().table_layout_update_scheduled.get()
            || self
                .ivars()
                .visible_table_layout_cache
                .borrow()
                .as_ref()
                .is_some_and(|cache| {
                    cache.request_key == request_key && !self.ivars().source_decorations_dirty.get()
                })
        {
            // An in-flight or already-current request requires no second dispatch block.
            return;
        }
        // Coalesce requests without laying out on the current stack. MainThreadBound carries
        // a weak view across dispatch safely without extending its native lifetime.
        if self.ivars().table_layout_update_scheduled.replace(true) {
            // Reentrant request acceptance preserves the first queued transaction.
            return;
        }
        let Some(mtm) = MainThreadMarker::new() else {
            // Thread confinement failure must not leave a permanently scheduled flag or queue a native callback.
            self.ivars().table_layout_update_scheduled.set(false);
            return;
        };
        let weak_view = MainThreadBound::new(Weak::from(self), mtm);
        enqueue_native_markdown_main_task(move || {
            // SAFETY: this closure executes only on the dispatch main queue, establishing main-thread access.
            let mtm = unsafe { MainThreadMarker::new_unchecked() };
            let Some(text_view) = weak_view.get(mtm).load() else {
                // The originating view is gone; the queued task has no owner or successor authority.
                return;
            };
            text_view.run_scheduled_table_layout_update();
        });
    }

    /// Consumes one scheduled derived-layout request after native editing permits element enumeration.
    pub(super) fn run_scheduled_table_layout_update(&self) {
        // A queued weak callback may still find a native shell retained by AppKit during close.
        // It cannot rebuild presentation or touch its disconnected canonical/storage graph.
        if self.ivars().textkit_graph_teardown.get() {
            // A previously queued executor clears its request after input/graph ownership has ended.
            self.ivars().table_layout_update_scheduled.set(false);
            return;
        }
        // TextKit can hold an outer content transaction after our own input-depth guard returns
        // to zero. Its public contract forbids layout or secondary enumeration during that edit.
        if self
            .textContentStorage()
            .is_some_and(|storage| storage.hasEditingTransaction())
        {
            // A later draw/input-completion request may schedule fresh work; never spin or recurse
            // inside an active TextKit editing transaction merely to populate derived overlays.
            self.ivars().table_layout_update_scheduled.set(false);
            return;
        }
        native_markdown_trace! {
            eprintln!(
                "[probe] run scheduled={} input_depth={} marked={:?}",
                self.ivars().table_layout_update_scheduled.get(),
                self.ivars().textkit_input_depth.get(),
                self.ivars().marked_text_range.get(),
            );
        }
        if !self.ivars().table_layout_update_scheduled.get() {
            // An explicit input/layout transaction already consumed or cancelled this queued request.
            return;
        }
        if self.ivars().textkit_input_depth.get() != 0
            || self
                .ivars()
                .marked_text_range
                .get()
                .is_some_and(|range| range.length != 0)
        {
            // Input completion or unmarkText will request layout again; this callback only
            // clears the coalescing flag while composition is active.
            self.ivars().table_layout_update_scheduled.set(false);
            return;
        }
        let cache_changed = self.refresh_visible_table_layout_cache();
        // Reuse the settled viewport transaction for bounded ruler geometry, never draw-time layout.
        self.refresh_source_gutter();
        let decorations_changed = self.refresh_source_decorations();
        native_markdown_trace! {
            eprintln!(
                "[probe] run after refresh manager={} content={} changed={}",
                self.textLayoutManager().is_some(),
                self.textContentStorage().is_some(),
                cache_changed
            );
        }
        self.reposition_active_table_cell_editor();
        native_markdown_trace! {
            eprintln!(
                "[probe] run after reposition manager={} content={}",
                self.textLayoutManager().is_some(),
                self.textContentStorage().is_some()
            );
        }
        let _ = self.activate_pending_table_cell();
        native_markdown_trace! {
            eprintln!(
                "[probe] run after pending manager={} content={}",
                self.textLayoutManager().is_some(),
                self.textContentStorage().is_some()
            );
        }
        self.ivars().table_layout_update_scheduled.set(false);
        if cache_changed || decorations_changed {
            // Publish prepared overlays in one subsequent draw; its current cache keys prevent idle rescheduling.
            self.setNeedsDisplay(true);
        }
        native_markdown_trace! {
            eprintln!(
                "[probe] run end manager={} content={}",
                self.textLayoutManager().is_some(),
                self.textContentStorage().is_some()
            );
        }
    }

    /// Replaces the exact generation/viewport cache once, including stable empty or bounded-failure results.
    pub(super) fn refresh_visible_table_layout_cache(&self) -> bool {
        if self.ivars().table_layout_rebuilding.get() {
            // Reentrant native drawing may inspect the old cache but cannot recursively rebuild it.
            return false;
        }
        self.ivars().table_layout_rebuilding.set(true);
        // The guard remains set across native geometry calls; all finite exits reset it below.
        let changed = (|| {
            if self.ivars().textkit_input_depth.get() != 0
                || self
                    .ivars()
                    .marked_text_range
                    .get()
                    .is_some_and(|range| range.length != 0)
            {
                // Provisional native input keeps source readable and defers table geometry until stable.
                return false;
            }
            let Some(request_key) = self.current_table_layout_request_key() else {
                // An ended canonical owner invalidates every cached table instead of retaining old source.
                return self
                    .ivars()
                    .visible_table_layout_cache
                    .borrow_mut()
                    .take()
                    .is_some();
            };
            let Some(text_storage) = self.ivars().text_storage.load() else {
                // Missing native storage is another stable empty result after owner teardown.
                return self
                    .ivars()
                    .visible_table_layout_cache
                    .borrow_mut()
                    .take()
                    .is_some();
            };
            let document_length = request_key.document_length;
            let visible_rect = self.visibleRect();
            let viewport_utf16_window = if document_length == 0 {
                // Empty documents need no TextKit element enumeration.
                0..0
            } else {
                // Use the current TextKit 2 viewport rather than flattening text for point mapping.
                self.visible_utf16_window(visible_rect, document_length)
            };
            native_markdown_trace! {
                eprintln!(
                    "[probe] refresh key={:?} viewport={:?} mode={:?}",
                    request_key, viewport_utf16_window, request_key.presentation_mode
                );
            }
            let snapshot = if request_key.presentation_mode
                == MarkdownPresentationMode::InlinePreview
                && document_length != 0
            {
                // Only preview mode requests bounded semantic table prototypes.
                text_storage.table_presentation_snapshot(viewport_utf16_window.clone())
            } else {
                // Source mode and empty text intentionally own no derived table overlays.
                None
            };
            native_markdown_trace! {
                eprintln!(
                    "[probe] snapshot={} window={:?}",
                    snapshot.as_ref().map_or(0, |value| value.tables.len()),
                    snapshot.as_ref().map(|value| value.utf16_window.clone())
                );
            }
            let text_view_width = self.bounds().size.width;
            let cache_matches = self
                .ivars()
                .visible_table_layout_cache
                .borrow()
                .as_ref()
                .is_some_and(|cache| cache.request_key == request_key);
            if cache_matches {
                // Repeated draw/layout notifications for an unchanged request do no additional work.
                return false;
            }

            let (utf16_window, tables) = if let Some(snapshot) = snapshot {
                // Reuse compatible column geometry while rebuilding only the currently visible cells.
                let tables = {
                    // Measurements call AppKit; retain a finite immutable snapshot and end RefCell access first.
                    let previous_cache = self.ivars().visible_table_layout_cache.borrow().clone();
                    self.build_visible_table_layouts(
                        &text_storage,
                        &snapshot,
                        &viewport_utf16_window,
                        visible_rect,
                        previous_cache.as_ref(),
                    )
                };
                native_markdown_trace! {
                    eprintln!("[probe] built tables={}", tables.len());
                }
                (snapshot.utf16_window, tables)
            } else {
                // No visible table is a stable result too. Cache that negative result so plain text,
                // source mode and bounded failures do not schedule work on every drawRect:.
                (viewport_utf16_window.clone(), Vec::new())
            };
            #[cfg(not(feature = "native-markdown-runtime-probe"))]
            let _ = &utf16_window;
            // Publish one complete immutable projection after all reentrant native measurements ended.
            *self.ivars().visible_table_layout_cache.borrow_mut() =
                Some(NativeMarkdownVisibleTableLayoutCache {
                    document_generation: request_key.document_generation,
                    #[cfg(feature = "native-markdown-runtime-probe")]
                    utf16_window,
                    #[cfg(feature = "native-markdown-runtime-probe")]
                    viewport_utf16_window,
                    text_view_width,
                    tables,
                    request_key,
                });
            true
        })();
        self.ivars().table_layout_rebuilding.set(false);
        changed
    }

    /// Measures and positions only viewport-intersecting table rows and their fixed overscan cells.
    pub(super) fn build_visible_table_layouts(
        &self,
        text_storage: &PieceBackedMarkdownTextStorage,
        snapshot: &NativeMarkdownTablePresentationSnapshot,
        viewport_utf16_window: &std::ops::Range<usize>,
        visible_rect: NSRect,
        previous_cache: Option<&NativeMarkdownVisibleTableLayoutCache>,
    ) -> Vec<NativeMarkdownVisibleTableLayout> {
        native_markdown_trace! {
            eprintln!(
                "[probe] build tables begin manager={} content={}",
                self.textLayoutManager().is_some(),
                self.textContentStorage().is_some()
            );
        }
        // Reserve the document inset and one row of vertical overscan before selecting table fragments.
        let text_origin = self.textContainerOrigin();
        let inset = self.textContainerInset();
        let available_width = (self.bounds().size.width - text_origin.x - inset.width).max(0.0);
        let expanded_visible_rect = NSRect::new(
            NSPoint::new(
                visible_rect.origin.x,
                visible_rect.origin.y - NATIVE_MARKDOWN_TABLE_ROW_HEIGHT,
            ),
            NSSize::new(
                visible_rect.size.width,
                visible_rect.size.height + NATIVE_MARKDOWN_TABLE_ROW_HEIGHT * 2.0,
            ),
        );
        let mut layouts = Vec::new();
        // Semantic parsing may include overscan tables; geometry is acquired only for visible intervals.
        for table in &snapshot.tables {
            // A parser overscan may contain distant tables. Reject those before asking
            // TextKit for their geometry: ensuring offscreen EOF fragments can move its
            // transient viewport range away from the actual scroll clip.
            if table.utf16_range.start >= viewport_utf16_window.end
                || table.utf16_range.end <= viewport_utf16_window.start
            {
                continue;
            }
            let visible_row_range =
                native_markdown_visible_table_row_range(&table.rows, viewport_utf16_window);
            native_markdown_trace! {
                eprintln!(
                    "[probe] table range={:?} rows={} visible_rows={:?}",
                    table.utf16_range,
                    table.rows.len(),
                    visible_row_range
                );
            }
            if visible_row_range.is_empty() {
                // A table with no current visible/overscan rows contributes no native cell resources.
                continue;
            }
            let source_rows: Vec<(usize, NSRect)> = visible_row_range
                .clone()
                .filter_map(|row_index| {
                    // Resolve row source geometry individually; missing transient fragments remain source text.
                    let row = table.rows.get(row_index)?;
                    let fragment = self.text_view_fragment_for_utf16_offset(row.utf16_range.start);
                    native_markdown_trace! {
                        eprintln!(
                            "[probe] build fragment offset={} manager={} content={} present={}",
                            row.utf16_range.start,
                            self.textLayoutManager().is_some(),
                            self.textContentStorage().is_some(),
                            fragment.is_some(),
                        );
                    }
                    fragment.map(|frame| (row_index, frame))
                })
                .collect();
            native_markdown_trace! {
                eprintln!("[probe] source_rows={}", source_rows.len());
            }
            if source_rows.is_empty() {
                // Unlaid-out rows cannot provide safe overlay hit targets; a later viewport event may retry.
                continue;
            }
            // Reject nonintersecting native row bounds before measuring any cell display strings.
            let minimum_y = source_rows
                .iter()
                .map(|(_, rect)| rect.origin.y)
                .fold(f64::INFINITY, f64::min);
            let maximum_y = source_rows
                .iter()
                .map(|(_, rect)| {
                    rect.origin.y + rect.size.height.max(NATIVE_MARKDOWN_TABLE_ROW_HEIGHT)
                })
                .fold(f64::NEG_INFINITY, f64::max);
            let provisional_frame = NSRect::new(
                NSPoint::new(text_origin.x, minimum_y),
                NSSize::new(available_width, (maximum_y - minimum_y).max(0.0)),
            );
            if !NSIntersectsRect(provisional_frame, expanded_visible_rect) {
                // Semantic overlap alone does not justify materializing native cells outside the clip.
                continue;
            }

            // Establish current semantic column count and retain only one bounded width per column.
            let column_count = table
                .cells
                .iter()
                .map(|cell| cell.column_index.saturating_add(1))
                .max()
                .unwrap_or(0);
            if column_count == 0 {
                // Empty parsed cells cannot produce a meaningful grid or editing coordinate.
                continue;
            }
            let mut natural_widths = vec![NATIVE_MARKDOWN_TABLE_MINIMUM_COLUMN_WIDTH; column_count];
            if let Some(previous_table) = previous_cache.and_then(|cache| {
                // Column widths can be reused only within the same generation, width and table identity.
                (cache.document_generation == snapshot.document_generation
                    && (cache.text_view_width - self.bounds().size.width).abs() < 0.5)
                    .then(|| {
                        cache.tables.iter().find(|candidate| {
                            candidate.table_utf16_start == table.utf16_range.start
                                && candidate.column_offsets.len() == column_count + 1
                        })
                    })?
            }) {
                // Preserve columns already measured in another visible part of this same table.
                for (column_index, offsets) in previous_table.column_offsets.windows(2).enumerate()
                {
                    natural_widths[column_index] = offsets[1] - offsets[0];
                }
            }
            // Capacity depends only on selected visible/overscan rows, never the whole table row count.
            let visible_cell_capacity =
                source_rows.iter().fold(0_usize, |count, (row_index, _)| {
                    count.saturating_add(
                        table
                            .rows
                            .get(*row_index)
                            .map_or(0, |row| row.cell_index_range.len()),
                    )
                });
            let mut prepared_cells = Vec::with_capacity(visible_cell_capacity);
            let options = NSStringDrawingOptions::UsesLineFragmentOrigin
                | NSStringDrawingOptions::TruncatesLastVisibleLine;
            // Native string measurement is limited to these visible rows and their existing cells.
            for (row_index, _) in &source_rows {
                let Some(row) = table.rows.get(*row_index) else {
                    // A missing semantic row cannot justify a stale source-range read.
                    continue;
                };
                // Prepare each visible cell once so drawing can use immutable attributed text only.
                for cell_index in row.cell_index_range.clone() {
                    let Some(cell) = table.cells.get(cell_index) else {
                        // Defensive malformed-prototype handling leaves the source cell readable.
                        continue;
                    };
                    let Some(attributed_text) =
                        text_storage.table_cell_attributed_text(&cell.utf16_range)
                    else {
                        // A bounded source/style failure creates no partial native cell object.
                        continue;
                    };
                    let measured = attributed_text.boundingRectWithSize_options_context(
                        NSSize::new(NATIVE_MARKDOWN_TABLE_MAXIMUM_COLUMN_WIDTH, f64::MAX),
                        options,
                        None,
                    );
                    natural_widths[cell.column_index] = natural_widths[cell.column_index].max(
                        measured.size.width + NATIVE_MARKDOWN_TABLE_CELL_HORIZONTAL_PADDING * 2.0,
                    );
                    prepared_cells.push((cell_index, *row_index, attributed_text, measured.size));
                }
            }
            let column_widths =
                native_markdown_table_column_widths(&natural_widths, available_width);
            if column_widths.len() != column_count {
                // A collapsed/invalid horizontal viewport has no consistent cell grid to expose.
                continue;
            }
            let mut column_offsets = Vec::with_capacity(column_count + 1);
            column_offsets.push(0.0);
            // Prefix sums define one shared edge coordinate for neighboring cells and grid lines.
            for width in &column_widths {
                let next = column_offsets.last().copied().unwrap_or(0.0) + width;
                column_offsets.push(next);
            }
            let table_width = column_offsets.last().copied().unwrap_or(0.0);
            let rows: Vec<NativeMarkdownVisibleTableRowLayout> = source_rows
                .iter()
                .enumerate()
                .filter_map(|(visible_index, (row_index, source))| {
                    // Neighboring native row origins determine height; the final visible row uses its fragment.
                    let row = table.rows.get(*row_index)?;
                    let next_y = source_rows
                        .get(visible_index + 1)
                        .map(|(_, next)| next.origin.y);
                    let height = next_y.filter(|next| *next > source.origin.y).map_or_else(
                        || source.size.height.max(NATIVE_MARKDOWN_TABLE_ROW_HEIGHT),
                        |next| next - source.origin.y,
                    );
                    Some(NativeMarkdownVisibleTableRowLayout {
                        row_index: *row_index,
                        frame: NSRect::new(
                            NSPoint::new(text_origin.x, source.origin.y),
                            NSSize::new(table_width, height),
                        ),
                        is_header: row.is_header,
                    })
                })
                .collect();
            let minimum_y = rows
                .iter()
                .map(|row| row.frame.origin.y)
                .fold(f64::INFINITY, f64::min);
            let maximum_y = rows
                .iter()
                .map(|row| row.frame.origin.y + row.frame.size.height)
                .fold(f64::NEG_INFINITY, f64::max);
            let cells = prepared_cells
                .into_iter()
                .filter_map(|(cell_index, row_index, attributed_text, measured_size)| {
                    // Translate each prepared cell into the current shared row/column coordinate system.
                    let prototype = table.cells.get(cell_index)?;
                    let row = table.rows.get(row_index)?;
                    let row_frame = rows
                        .iter()
                        .find(|candidate| candidate.row_index == row_index)?
                        .frame;
                    let column_start = *column_offsets.get(prototype.column_index)?;
                    let column_end = *column_offsets.get(prototype.column_index + 1)?;
                    let frame = NSRect::new(
                        NSPoint::new(text_origin.x + column_start, row_frame.origin.y),
                        NSSize::new(column_end - column_start, row_frame.size.height),
                    );
                    let available_text_width = (frame.size.width
                        - NATIVE_MARKDOWN_TABLE_CELL_HORIZONTAL_PADDING * 2.0)
                        .max(0.0);
                    let available_text_height = (frame.size.height
                        - NATIVE_MARKDOWN_TABLE_CELL_VERTICAL_PADDING * 2.0)
                        .max(0.0);
                    let text_width = measured_size.width.min(available_text_width).max(1.0);
                    let text_height = measured_size.height.min(available_text_height).max(1.0);
                    let text_x = match prototype.alignment {
                        MarkdownTableAlignment::Left => {
                            // Left alignment retains fixed leading padding inside the bounded cell.
                            frame.origin.x + NATIVE_MARKDOWN_TABLE_CELL_HORIZONTAL_PADDING
                        }
                        MarkdownTableAlignment::Center => {
                            // Centering uses the clipped measured display width, not source-byte count.
                            frame.origin.x + (frame.size.width - text_width) / 2.0
                        }
                        MarkdownTableAlignment::Right => {
                            // Right alignment reserves the same fixed trailing padding.
                            frame.origin.x + frame.size.width
                                - NATIVE_MARKDOWN_TABLE_CELL_HORIZONTAL_PADDING
                                - text_width
                        }
                    };
                    Some(NativeMarkdownVisibleTableCellLayout {
                        coordinate: NativeMarkdownTableCellCoordinate {
                            table_utf16_start: table.utf16_range.start,
                            row_index,
                            column_index: prototype.column_index,
                        },
                        source_utf16_range: prototype.utf16_range.clone(),
                        frame,
                        text_frame: NSRect::new(
                            NSPoint::new(
                                text_x,
                                frame.origin.y + (frame.size.height - text_height) / 2.0,
                            ),
                            NSSize::new(text_width, text_height),
                        ),
                        is_header: row.is_header,
                        alignment: prototype.alignment,
                        attributed_text,
                    })
                })
                .collect();
            // Retain only the completed visible projection; the original Markdown remains in the piece tree.
            layouts.push(NativeMarkdownVisibleTableLayout {
                table_utf16_start: table.utf16_range.start,
                frame: NSRect::new(
                    NSPoint::new(text_origin.x, minimum_y),
                    NSSize::new(table_width, (maximum_y - minimum_y).max(0.0)),
                ),
                rows,
                column_offsets,
                cells,
            });
        }
        native_markdown_trace! {
            eprintln!(
                "[probe] build tables end manager={} content={} layouts={}",
                self.textLayoutManager().is_some(),
                self.textContentStorage().is_some(),
                layouts.len(),
            );
        }
        layouts
    }

    /// Reads TextKit 2 viewport geometry without forcing legacy point-lookup or whole-document layout.
    pub(super) fn visible_utf16_window(
        &self,
        visible_rect: NSRect,
        document_length: usize,
    ) -> std::ops::Range<usize> {
        if document_length == 0 {
            // No document text means no scalar range or native viewport fragments to enumerate.
            return 0..0;
        }
        // Reconcile the current scroll clip in this asynchronous layout transaction.
        // Explicit attribute/fragment queries can leave the controller's previous range stale.
        if let Some(manager) = self.textLayoutManager() {
            // Consume the current visible viewport only; never ask a legacy manager to lay out the full note.
            manager.textViewportLayoutController().layoutViewport();
        }
        // Query TextKit 2 fragments in container coordinates, independent of the
        // controller's transient range after an explicit offscreen layout request.
        if let Some(manager) = self.textLayoutManager()
            && let Some(content) = manager.textContentManager()
        {
            // Prefer fragments at the physical clip edges, retaining complete visible paragraphs for semantic projection.
            let origin = self.textContainerOrigin();
            let top = NSPoint::new(5.0, (visible_rect.origin.y - origin.y).max(0.0));
            let bottom = NSPoint::new(
                5.0,
                (visible_rect.origin.y + visible_rect.size.height - origin.y - 1.0).max(0.0),
            );
            if let Some(first) = manager.textLayoutFragmentForPosition(top)
                && let Some(last) = manager.textLayoutFragmentForPosition(bottom)
            {
                // Both edge fragments are required; one missing fragment cannot define a safe viewport interval.
                let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(&*content);
                let start_location = provider.documentRange().location();
                let start = content.offsetFromLocation_toLocation(
                    &start_location,
                    &first.rangeInElement().location(),
                );
                let end = content.offsetFromLocation_toLocation(
                    &start_location,
                    &last.rangeInElement().endLocation(),
                );
                // Fragment ranges include complete visible paragraphs and remain scalar aligned.
                if let (Ok(start), Ok(end)) = (usize::try_from(start), usize::try_from(end))
                    && start < end
                    && start < document_length
                {
                    // Validated source coordinates cannot exceed the current canonical document's EOF.
                    return start..end.min(document_length);
                }
            }
        }
        // TextKit 2's viewport range is authoritative. The legacy insertion-point API
        // may transiently return EOF for every point while its compatibility manager is detached.
        if let Some(manager) = self.textLayoutManager()
            && let Some(content) = manager.textContentManager()
            && let Some(viewport) = manager.textViewportLayoutController().viewportRange()
        {
            // The controller's public range is a secondary source when explicit point fragments are not ready.
            let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(&*content);
            let start_location = provider.documentRange().location();
            let start =
                content.offsetFromLocation_toLocation(&start_location, &viewport.location());
            let end =
                content.offsetFromLocation_toLocation(&start_location, &viewport.endLocation());
            // Accept only a non-empty current-document range; unavailable geometry falls back below.
            if let (Ok(start), Ok(end)) = (usize::try_from(start), usize::try_from(end))
                && start < end
                && start < document_length
            {
                // A current nonempty range permits bounded semantic work without requesting offscreen layout.
                return start..end.min(document_length);
            }
        }
        // No valid TextKit 2 geometry is available in this transaction. A legacy
        // insertion-point query would switch the view to TextKit 1; defer table
        // overlays until the next native layout event instead of changing engines.
        0..0
    }

    /// Obtains one TextKit 2 fragment frame for a scalar-aligned source offset.
    pub(super) fn text_view_fragment_for_utf16_offset(
        &self,
        utf16_offset: usize,
    ) -> Option<NSRect> {
        let text_layout_manager = match self.textLayoutManager() {
            // The surface explicitly retains the normal manager throughout this foreground query.
            Some(value) => value,
            // A disconnected native shell cannot generate table overlays or regain a manager during teardown.
            None => {
                native_markdown_trace! {
                    eprintln!("[probe] fragment offset={} no layout manager", utf16_offset);
                }
                return None;
            }
        };
        let text_content_manager = match text_layout_manager.textContentManager() {
            // The manager's live content owner supplies canonical source-to-native location conversion.
            Some(value) => value,
            // Detached content has no authoritative native coordinate space for an overlay.
            None => {
                native_markdown_trace! {
                    eprintln!(
                        "[probe] fragment offset={} no content manager",
                        utf16_offset
                    );
                }
                return None;
            }
        };
        let element_provider =
            ProtocolObject::<dyn NSTextElementProvider>::from_ref(&*text_content_manager);
        let document_range = element_provider.documentRange();
        let document_start = document_range.location();
        let offset = isize::try_from(utf16_offset).ok()?;
        let location =
            match text_content_manager.locationFromLocation_withOffset(&document_start, offset) {
                // An existing source boundary is safe to submit as a finite point layout request.
                Some(value) => value,
                // A transiently unavailable scalar location leaves ordinary source rendering in charge.
                None => {
                    native_markdown_trace! {
                        eprintln!("[probe] fragment offset={} no location", utf16_offset);
                    }
                    return None;
                }
            };
        let point_range = NSTextRange::initWithLocation(NSTextRange::alloc(), &location);
        text_layout_manager.ensureLayoutForRange(&point_range);
        let fragment = match text_layout_manager.textLayoutFragmentForLocation(&location) {
            // Use only the requested fragment's prepared frame, never a document-wide layout walk.
            Some(value) => value,
            // Native layout may omit folded/unavailable text; absent geometry produces no stale cell target.
            None => {
                native_markdown_trace! {
                    eprintln!(
                        "[probe] fragment offset={} lookup returned none",
                        utf16_offset
                    );
                }
                return None;
            }
        };
        let frame = fragment.layoutFragmentFrame();
        native_markdown_trace! {
            eprintln!("[probe] fragment offset={} frame={:?}", utf16_offset, frame);
        }
        // Reserve the document inset and one row of vertical overscan before selecting table fragments.
        let text_origin = self.textContainerOrigin();
        Some(NSRect::new(
            NSPoint::new(
                text_origin.x + frame.origin.x,
                text_origin.y + frame.origin.y,
            ),
            frame.size,
        ))
    }
}
