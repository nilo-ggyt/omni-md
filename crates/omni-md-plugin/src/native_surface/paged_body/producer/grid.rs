//! Stream table semantic certification through the original request; glyphs advance the actual viewport
//! prefix using the body unique font worker.
//!
//! Return completed offscreen cell resources; backward scrolling reconstructs the actual current
//! viewport from the original sealed row directory.

use super::super::scene::{GridCellContent, GridCellPublication, GridRowPublication};
mod target;
use super::*;
use crate::native_surface::{
    NATIVE_MARKDOWN_TABLE_CELL_HORIZONTAL_PADDING as HORIZONTAL,
    NATIVE_MARKDOWN_TABLE_CELL_VERTICAL_PADDING as VERTICAL,
    NATIVE_MARKDOWN_TABLE_MAXIMUM_COLUMN_WIDTH as MAXIMUM_WIDTH,
    NATIVE_MARKDOWN_TABLE_ROW_HEIGHT as MINIMUM_HEIGHT,
    paged_line_viewport::NativeMarkdownComposedTableCell,
    table_geometry::{native_markdown_table_column_widths, native_markdown_table_content_width},
};
use objc2_foundation::{NSRect, NSSize};
use omni_markdown_core::markdown_document_buffer::SemanticWork;
use omni_markdown_core::markdown_document_buffer::VerifiedMarkdownTableCellSource;
use omni_markdown_core::{
    MarkdownLineTableRowRequest, MarkdownTableCellDisplayPreparation,
    MarkdownTableCellDisplayPreparationStep, MarkdownTableCellDisplaySource,
};
use std::ops::Range;
use target::{CellTarget, RowTarget};
pub(super) mod word;

/// Measure this turn actual cell work only, separating display deltas from Composer quanta because the
/// original Request already accounts CellInline.
#[derive(Default)]
pub(super) struct Work {
    /// Display-conversion work absent from the original request total; do not count its
    /// already-accounted CellInline work again.
    pub display: SemanticWork,
    /// Actual Composer reads are not unique file bytes; independent Unicode subtasks may read the same
    /// range separately.
    pub bytes: usize,
    /// Actual Unicode/shaping cursor progress describes work instead of estimating it from text length.
    pub scalars: usize,
    /// Actual directory records use different units from Source reads and glyph counts.
    pub metadata: usize,
    /// Maxima come from one original step, rather than treating a sum of serial turns as a single-turn
    /// allowance.
    pub maximum_bytes: usize,
    pub maximum_scalars: usize,
    pub maximum_metadata: usize,
    /// The outer original FontTrial accounts trial reads separately; include original frontier Source
    /// work only here.
    pub frontier_source_bytes: usize,
    /// Actual shaper input is distinct from Source scanning/copying, which cannot claim completed
    /// shaping.
    pub shaped_input_bytes: usize,
}

/// Keep one physical table column widths fixed within their original width epoch, without reallocating
/// widths when later rows scroll.
pub(super) struct TableWidths {
    pub(super) header: usize,
    pub(super) widths: Rc<[f64]>,
}

/// Route actual cell requests to the Producer unique FontTrial/FontWorker, without a per-cell thread or
/// Body.
pub(super) enum Advance {
    Progress,
    Style {
        column: usize,
        request: MarkdownLineStyleRequest,
    },
    Control {
        column: usize,
        request: omni_markdown_core::MarkdownLineInlineControl,
    },
    Visible,
    Complete,
    Target {
        request: Rc<super::super::target::SourceTargetRequest>,
        rect: NSRect,
    },
}

/// Respond to the original Composer TableRow only after every final cell height completes; a visible
/// prefix is insufficient.
pub(super) struct Grid {
    /// The original Core nonce shares the structural ticket lifetime; final geometry answers this
    /// outstanding request only.
    request: Option<MarkdownLineTableRowRequest>,
    /// A restored row still needs an actual current same-Source structural ticket, without an
    /// outstanding Core response.
    row: omni_markdown_core::VerifiedMarkdownLayoutTableRow,
    /// Only an actually sealed output row has final height; cell-prefix EOF remains a separate fact.
    sealed_height: Option<f64>,
    /// Y-down document position sums actual earlier LineEnd heights, rather than character count or
    /// estimated lines.
    pub(super) top: f64,
    /// Final column boxes belong to the original width epoch and are quantized once to 26.6 for both
    /// Composer and Scene.
    widths: Rc<[f64]>,
    /// Measure first-header natural widths only; stop a cell at its actual cap without shaping all
    /// glyphs of a huge cell.
    natural: Option<NaturalWidths>,
    /// Column count comes from the original structural ticket; cells live with the visible row or
    /// unique unsubmitted row only.
    cells: Vec<Cell>,
    /// Rotate actual row columns so a huge first-column paragraph cannot starve later visible columns.
    current: usize,
    /// This flag means final geometry was returned only; Core must still finish
    /// structural/grapheme/directory work before LineReady.
    pub(super) submitted: bool,
    /// Find retains original weak business authority and a finite cell cursor, without another Input,
    /// document, or Source.
    target: Option<RowTarget>,
    /// Return actual work to the outer owner after advance so retiring natural-width work cannot lose
    /// performed reads.
    work: Work,
}

/// Natural-width measurement serially reuses one cell; reaching the cap needs no full huge-cell layout
/// to prove a larger width.
struct NaturalWidths {
    values: Vec<f64>,
    active: Cell,
}

/// An actually empty cell creates no Composer; nonempty cells own actual continuations/output needed by
/// the current visible row.
struct Cell {
    /// Work coordinates grant no Source authority; actual ranges come from original structural tickets
    /// and display Source.
    column: usize,
    /// Borrow actual CellInline from the original request, then convert in 4 KiB steps; Pending grants
    /// no readable display Source.
    preparation: Option<MarkdownTableCellDisplayPreparation>,
    /// Read-only display directories and canonical mapping are neither writable drafts nor members of
    /// the body Input domain.
    display: Option<Rc<MarkdownTableCellDisplaySource>>,
    /// Only empty raw range plus completed empty display permits this branch; unknown nonempty cells
    /// cannot masquerade as empty.
    empty: Option<VerifiedMarkdownTableCellSource>,
    /// Share the body FontCatalog and own this actual display Source line directory, without a per-cell
    /// Body or thread.
    composer: Option<MarkdownLineComposer>,
    /// Revoke original Core subtasks at retirement so late work cannot use old Source to create new
    /// glyphs.
    cancellation: MarkdownLineComposeCancellation,
    /// Scene shares the same prefix Reader; cloning Rc opens no additional directory descriptor.
    output: Option<Rc<RefCell<MarkdownComposedLines>>>,
    /// A sealed line may have many parts; resume its unfinished actual font metrics at the same cursor.
    pending: Option<LineMetrics>,
    /// Sum completely measured lines only; unfinished pending lines cannot contribute early height.
    height: f64,
    lines: usize,
    /// Only actual Composer EOF or verified raw/display emptiness completes the cell; viewport coverage
    /// cannot set this flag.
    complete: bool,
    width: f64,
    natural_line: f64,
    natural_maximum: f64,
    scan: Option<CellScan>,
    target: Option<CellTarget>,
}

/// Keep line directories in the original prefix Reader; window preparation retains bounded weak handles
/// and the next actual metric cursor.
struct CellScan {
    /// Each geometry request prepares its own window; old width/scroll authority cannot certify a
    /// successor Scene Ready.
    request: NativeMarkdownPagedSceneRequest,
    /// Handles weakly identify the original Reader without moving offscreen glyphs or whole-cell text
    /// into Native arrays.
    cursor: Option<MarkdownComposedLine>,
    metrics: Option<LineMetrics>,
    bottom: f64,
    first_top: f64,
    handles: VecDeque<NativeMarkdownMeasuredLine>,
    ready: bool,
}

impl Grid {
    /// Core accepted the original block start and NeedStyle nonce; do not construct rows or Input
    /// graphs from bare ranges.
    pub(super) fn new(
        request: MarkdownLineTableRowRequest,
        top: f64,
        widths: &Option<TableWidths>,
    ) -> Self {
        let columns = request.row().columns();
        let header = request.row().coordinate().0;
        let retained = widths.as_ref().filter(|widths| widths.header == header);
        // Reuse solved widths for later rows in the same table instead of reallocating positions
        // whenever scrolling downward.
        // These widths belong to the original Producer/width epoch and cannot cross width supersession
        // or another table.
        let (widths, natural) = match retained {
            Some(retained) => (retained.widths.clone(), None),
            None => {
                // Sequential flow must reach the actual header first; violating this assembly rule
                // cannot be hidden by default widths.
                // Restoration reads actual sealed column widths instead of silently remeasuring an
                // arbitrary body row.
                assert_eq!(
                    request.row().coordinate().1,
                    0,
                    "顺序 flow 先经过原真实表头"
                );
                (
                    Rc::from([]),
                    Some(NaturalWidths {
                        values: Vec::with_capacity(columns),
                        active: Cell::new(0, MAXIMUM_WIDTH),
                    }),
                )
            }
        };
        let cells = if natural.is_none() {
            // Allocate bounded column metadata only; create actual Source/Composer when that column
            // advances.
            // Many source cells do not authorize assembling whole-table glyphs or Native input objects
            // eagerly.
            widths
                .iter()
                .enumerate()
                .map(|(column, width)| Cell::new(column, *width))
                .collect()
        } else {
            Vec::new()
        };
        let row = request.row().clone();
        Self {
            request: Some(request),
            row,
            sealed_height: None,
            top,
            widths,
            natural,
            cells,
            current: 0,
            submitted: false,
            target: None,
            work: Work::default(),
        }
    }

    /// Accept original Reader-certified final height/widths only; restoration fabricates no outstanding
    /// TableRowRequest.
    pub(super) fn restored(
        row: omni_markdown_core::VerifiedMarkdownLayoutTableRow,
        top: f64,
        height: f64,
        widths: Rc<[f64]>,
    ) -> Self {
        let cells = widths
            .iter()
            .enumerate()
            .map(|(column, width)| Cell::new(column, *width))
            .collect();
        Self {
            request: None,
            row,
            sealed_height: Some(height),
            top,
            widths,
            natural: None,
            cells,
            current: 0,
            submitted: true, // The original row was actually sealed; this object owes no new Core response.
            target: None,
            work: Work::default(),
        }
    }

    /// Outer original authority guards Native reentry; pure cell work additionally checks the same
    /// structural row and canonical Source.
    fn require(&self, owner: &Owner, source: &MarkdownLayoutTextSource) -> Result<()> {
        // Borrow the document briefly for pure Core source checks and release before later font/window
        // callbacks may reenter.
        // A new publication with equal numeric ranges cannot renew an old structural row ticket.
        let document = owner
            .document
            .borrow()
            .as_ref()
            .expect("表格持原Body文档")
            .clone();
        self.row.require_for(&document.borrow(), source)?;
        Ok(())
    }

    /// The unique current FontTrial names the actual in-flight cell, without recapturing Source by
    /// column or switching coordinate domains.
    pub(super) fn display(&self, column: usize) -> Rc<MarkdownTableCellDisplaySource> {
        self.cell(column)
            .display
            .as_ref()
            .expect("格Font请求只在真实显示源完成后产生")
            .clone()
    }
    pub(super) fn source(&self, column: usize) -> &MarkdownLayoutTextSource {
        self.cell(column)
            .display
            .as_ref()
            .expect("Cell请求持真实显示源")
            .source()
    }

    /// Transfer performed reads to the original Producer before cell retirement so temporary
    /// natural-width work cannot discard them.
    pub(super) fn take_work(&mut self) -> Work {
        std::mem::take(&mut self.work)
    }

    /// Return the same-request response to its original cell Composer rather than injecting a
    /// cell-local range into the body Composer.
    pub(super) fn composer(&mut self, column: usize) -> &mut MarkdownLineComposer {
        self.cell_mut(column)
            .composer
            .as_mut()
            .expect("活Font/control请求持原格Composer")
    }

    fn cell(&self, column: usize) -> &Cell {
        match &self.natural {
            Some(natural) => {
                // Measurement owns one active cell; asynchronous font responses must return to that
                // cell rather than its successor column.
                assert_eq!(natural.active.column, column);
                &natural.active
            }
            None => &self.cells[column],
        }
    }
    fn cell_mut(&mut self, column: usize) -> &mut Cell {
        match &mut self.natural {
            Some(natural) => {
                // A mismatched work index is an assembly error, without a default cell or recaptured
                // display Source.
                assert_eq!(natural.active.column, column);
                &mut natural.active
            }
            None => &mut self.cells[column],
        }
    }

    /// Only completed actual cell LineEnds contribute height; unlaid suffixes or unseen cells cannot
    /// certify the final maximum.
    pub(super) fn height(&self) -> f64 {
        if let Some(height) = self.sealed_height {
            return height; // Original sealed geometry does not shrink to a restored visible prefix, preserving later body positions.
        }
        // Cells share the original minimum 38-point box; taller content increases the prefix only after
        // actual line metrics complete.
        // Before complete this maximum remains a lower bound and cannot position subsequent body
        // content.
        self.cells.iter().fold(MINIMUM_HEIGHT, |height, cell| {
            height.max((cell.height + VERTICAL * 2.0).ceil())
        })
    }

    pub(super) fn complete(&self) -> bool {
        // Finished measurement fixes column positions only, without certifying complete glyph layout at
        // final widths.
        // Final height requires every actual column to finish, not merely currently visible short
        // cells.
        self.natural.is_none() && self.cells.iter().all(|cell| cell.complete)
    }

    /// Fix widths in 26.6 only after actual natural-width solving; an unsolved empty width set grants
    /// no extra horizontal range.
    /// Restored rows obtain the same widths from the sealed directory, without estimates by column
    /// count or average characters during scroll.
    pub(super) fn width(&self) -> f64 {
        self.widths.iter().sum()
    }

    /// Check missing actual prefixes in visible columns only; sealed row height cannot certify
    /// readability of unrestored text.
    pub(super) fn needs_visible_work(
        &self,
        scene: &NativeMarkdownPagedSceneRequest,
        padding: f64,
    ) -> bool {
        let needed = (scene.virtual_viewport.document_origin.y + scene.viewport.size.height
            - self.top
            - VERTICAL)
            .max(0.0);
        self.cells.iter().enumerate().any(|(column, cell)| {
            self.column_visible(column, scene, padding)
                && !cell.complete
                && (cell.pending.is_some() || cell.output.is_none() || cell.height < needed)
        })
    }

    /// Retire in-flight semantic preparation only after the original request cell slot returns; a
    /// cancellation flag is not actual completion.
    pub(super) fn has_preparation(&self) -> bool {
        self.natural
            .as_ref()
            .is_some_and(|natural| natural.active.preparation.is_some())
            || self.cells.iter().any(|cell| cell.preparation.is_some())
    }

    /// Do not retake an already located original Find request; new requests still need the cell actual
    /// first-scalar line geometry.
    pub(super) fn needs_target(
        &self,
        target: &Rc<super::super::target::SourceTargetRequest>,
    ) -> bool {
        self.source_range().contains(&target.range.location)
            && !self
                .target
                .as_ref()
                .is_some_and(|old| Rc::ptr_eq(&old.request, target))
    }

    /// Observe current actual resources without retaining Source, files, tasks, or historical retired
    /// cells.
    pub(super) fn resource_receipts(
        &self,
    ) -> Result<Vec<super::super::NativeMarkdownPagedCellResourceReceipts>> {
        let cells = self
            .cells
            .iter()
            .chain(self.natural.iter().map(|natural| &natural.active));
        cells.map(Cell::resource_receipt).collect()
    }

    /// Keep full-row completion distinct from the measured prefix; pause shaping a huge cell at the
    /// visible bottom.
    pub(super) fn advance(
        &mut self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        scene: &NativeMarkdownPagedSceneRequest,
        word_target: Option<
            crate::native_surface::paged_text_view::NativeMarkdownPagedCellWordTarget,
        >,
        widths: &mut Option<TableWidths>,
        semantics: &mut super::super::semantics::RootLeaf,
    ) -> Result<Advance> {
        self.require(owner, source)?;
        if let Some(natural) = self.natural.as_mut() {
            // Display certification uses the same quantum; this branch never also advances the
            // final-width Composer in that turn.
            // Actual glyph advance establishes the cap, rather than character count or font name.
            let result = natural.active.advance(
                owner,
                source,
                &self.row,
                true,
                semantics,
                &mut self.work,
            )?;
            if natural.active.complete || natural.active.natural_maximum >= MAXIMUM_WIDTH {
                // Column width includes horizontal padding and borders draw inside the box, without
                // adding border width to the directory sum.
                let width = natural.active.natural_maximum.min(MAXIMUM_WIDTH) + HORIZONTAL * 2.0;
                natural.values.push(width);
                // This task certifies capped natural width only; cancelling remaining glyph work
                // changes neither original text nor semantics.
                natural.active.release_layout();
                if natural.values.len() == self.row.columns() {
                    // Narrow windows retain usable actual cell widths, with excess width belonging to
                    // the table horizontal extent.
                    // Never force many columns inside their padding and lose contents through
                    // zero-width cells.
                    let body_width =
                        super::width(scene, owner.configuration.padding)? as f64 / 64.0;
                    let available = body_width.max(natural.values.len() as f64 * 42.0);
                    let mut resolved =
                        native_markdown_table_column_widths(&natural.values, available);
                    // Quantize actual column boxes to directory 26.6 units first so sealed sums agree
                    // exactly with later painting.
                    for width in &mut resolved {
                        *width = (*width * 64.0).round() / 64.0;
                    }
                    self.widths = resolved.into();
                    *widths = Some(TableWidths {
                        header: self.row.coordinate().0,
                        widths: self.widths.clone(),
                    });
                    self.cells = self
                        .widths
                        .iter()
                        .enumerate()
                        .map(|(column, width)| Cell::new(column, *width))
                        .collect();
                    self.natural = None;
                } else {
                    // Create the next cell after the previous Source/directory retires, without
                    // simultaneous whole-row measurement output.
                    natural.active = Cell::new(natural.values.len(), MAXIMUM_WIDTH);
                }
                return Ok(Advance::Progress);
            }
            return Ok(result);
        }
        if let Some(column) = self
            .cells
            .iter()
            .position(|cell| cell.preparation.is_some())
        {
            // All cells share the original Request unique CellInline slot; while one column prepares
            // semantics/display,
            // starting another column preparation would supersede the actual original publication.
            // Deliver this original preparation as display Source before resuming glyph rotation,
            // without another parser or owner.
            return self.cells[column].advance(
                owner,
                source,
                &self.row,
                false,
                semantics,
                &mut self.work,
            );
        }
        if self.natural.is_none() && self.cells.iter().all(|cell| cell.complete) {
            // Find may target an already completed cell; locate its actual text line first instead of
            // returning the whole row-height box.
            if let Some(step) = self.advance_source_target(owner, source, scene, semantics)? {
                return Ok(step);
            }
            return Ok(Advance::Complete);
        }
        if let Some(step) = self.advance_source_target(owner, source, scene, semantics)? {
            // A distant target may exceed the viewport; shape through its actual first-scalar line
            // only, without finishing the match tail.
            return Ok(step);
        }
        // A measured mode anchor may lie inside a very tall cell with actual visible-prefix work still
        // missing beneath it.
        // In-flight Grid runs before ordinary line targets, so resume that cell here rather than
        // repeatedly publishing an old short clip prefix.
        let mode_bottom = owner.source_target.borrow().as_ref().and_then(|target| {
            if !target.pending()
                || target.source != source.authority()
                || !target.matches(owner, scene)
                || !owner.mode_retains_measured_glyph(target)
            {
                return None; // Ordinary Find or revoked mode authority cannot extend this actual visible-prefix request.
            }
            owner.mode_viewport_required_bottom(target, self.top, scene.viewport.size.height)
        });
        let required_bottom = (scene.virtual_viewport.document_origin.y
            + scene.viewport.size.height)
            .max(mode_bottom.unwrap_or(0.0));
        let needed = (required_bottom - self.top - VERTICAL).max(0.0);
        // Prepare the same vertical viewport for actual visible columns first, retaining at most one
        // additional offscreen work item.
        for _ in 0..self.cells.len() {
            let column = self.current;
            self.current = (self.current + 1) % self.cells.len();
            if self.cells[column].complete
                || self.cells[column].pending.is_none()
                    && self.cells[column].output.is_some()
                    && self.cells[column].height >= needed
            {
                // Finished short cells need remaining background only; long cells reaching the viewport
                // bottom keep their cursor for later scroll.
                continue;
            }
            if !self.column_visible(column, scene, owner.configuration.padding) {
                // Without visible ink outside the horizontal clip, create no Native cell window or
                // pixel buffer there.
                continue;
            }
            return self.cells[column].advance(
                owner,
                source,
                &self.row,
                false,
                semantics,
                &mut self.work,
            );
        }
        let target_follows = word_target
            .is_some_and(|target| target.row_start >= self.source_range().end)
            || owner.source_target.borrow().as_ref().is_some_and(|target| {
                target.pending()
                    && target.source == source.authority()
                    && target.matches(owner, scene)
                    && target.range.location >= self.source_range().end
            });
        // When the original target lies in a later structural row, filling the old viewport does not
        // complete navigation.
        // Obtain actual final height in bounded quanta and return it to the unique body Composer before
        // advancing to the next row;
        // premature Visible would republish the old first screen and incorrectly certify a distant Find
        // first character as visible.
        // A sealed restored row owes no new response and may return its bounded restoration work even
        // when the target follows it.
        if self.sealed_height.is_some()
            || !target_follows && self.height() + self.top >= required_bottom
        {
            // Certify the measured prefix only; unfinished rows keep Core suspended without a
            // fabricated LineReady.
            return Ok(Advance::Visible);
        }
        // If visible columns finish without known row end, measure the next actual offscreen column
        // serially instead of guessing the maximum.
        for column in 0..self.cells.len() {
            if !self.cells[column].complete {
                // Continue the original column when final row end is required; do not advance
                // subsequent text before that actual content ends.
                return self.cells[column].advance(
                    owner,
                    source,
                    &self.row,
                    false,
                    semantics,
                    &mut self.work,
                );
            }
        }
        Ok(Advance::Complete)
    }

    /// Horizontal extents sum actual column widths; clipping cannot use another column empty content as
    /// this column.
    fn column_visible(
        &self,
        column: usize,
        scene: &NativeMarkdownPagedSceneRequest,
        padding: f64,
    ) -> bool {
        let left = scene.virtual_viewport.layout_origin_x
            + padding
            + self.widths[..column].iter().sum::<f64>()
            - scene.virtual_viewport.document_origin.x;
        left < scene.viewport.origin.x + scene.viewport.size.width
            && left + self.widths[column] > scene.viewport.origin.x
    }

    /// Outer None means no actual cell-display coverage; Some(None) means the original parser certified
    /// no link at this location.
    /// Keep them distinct so unknown cells cannot swallow the first click and covered cells cannot be
    /// reinterpreted by ordinary RootLeaf.
    pub(super) fn link_at(
        &self,
        owner: &Owner,
        index: usize,
    ) -> std::result::Result<
        Option<Option<omni_markdown_core::MarkdownSourceLink>>,
        omni_markdown_core::markdown_document_buffer::MarkdownTableRequestError,
    > {
        let document = owner
            .document
            .borrow()
            .as_ref()
            .expect("格链接持原Body文档")
            .clone();
        let document = document.borrow();
        for cell in &self.cells {
            if let Some(display) = &cell.display {
                // Raw authority must still match the live document; old edited cells cannot open links
                // through numeric range intersection alone.
                let range = display
                    .canonical_raw_cell()
                    .canonical_range_for(&document)?;
                if range.contains(&index) {
                    // Keep original parser decoding, reference definitions, and label semantics rather
                    // than guessing links from displayed URLs.
                    return display
                        .link_at_for(&document, index)
                        .map(|link| Some(link.cloned()));
                }
            }
        }
        Ok(None)
    }

    /// Return the original physical table-row range including its actual EOL, without substituting
    /// visible cell text length for Source coordinates.
    pub(super) fn source_range(&self) -> Range<usize> {
        self.row.source_range()
    }

    /// Visibility uses the actual height prefix; estimated height cannot position later text past an
    /// unknown row end.
    pub(super) fn visible(&self, scene: &NativeMarkdownPagedSceneRequest) -> bool {
        self.top < scene.virtual_viewport.document_origin.y + scene.viewport.size.height
            && self.top + self.height() > scene.virtual_viewport.document_origin.y
    }

    /// Rows share this caller-turn metadata allowance; completed scans are not repeated in ways that
    /// starve later rows.
    pub(super) fn scan_windows(
        &mut self,
        owner: &Owner,
        request: &NativeMarkdownPagedSceneRequest,
        budget: &mut MarkdownLineComposeBudget,
        work: &mut Work,
    ) -> Result<bool> {
        let before_text = request.virtual_viewport.document_origin.y + request.viewport.size.height
            <= self.top + VERTICAL;
        for column in 0..self.cells.len() {
            if !self.column_visible(column, request, owner.configuration.padding) {
                // Build no Native window for horizontally offscreen columns, retaining actual positions
                // for later horizontal viewports.
                continue;
            }
            let cell = &mut self.cells[column];
            if cell.empty.is_some() {
                // Actual empty cells need background and canonical click authority only, without glyph
                // directories or empty Composers.
                continue;
            }
            if before_text && cell.display.is_some() {
                // The viewport contains top padding only while actual text remains outside; preserve
                // original cell authority for clicks,
                // without demanding full cell completion or creating an empty-clipped Native text
                // window.
                cell.scan = None;
                continue;
            }
            if cell.output.is_none() {
                // Without an actual LineEnd in a nonempty cell, keep the frame Pending rather than
                // presenting Empty or short-cell whitespace.
                return Ok(false);
            }
            if !cell.scan_window(owner, request, self.top, budget, work)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Complete actual directory windows before constructing Native cells, preventing earlier cells
    /// from repeatedly consuming later-cell allowance.
    pub(super) fn prepare(
        &mut self,
        owner: &Owner,
        key: &NativeMarkdownPagedSceneKey,
        current: &dyn Fn() -> bool,
    ) -> Result<GridRowPublication> {
        let row_height = self.height();
        let request = &key.request;
        let before_text = request.virtual_viewport.document_origin.y + request.viewport.size.height
            <= self.top + VERTICAL;
        // Source uses Y-down document coordinates; convert Native frames to Y-up coordinates within
        // this viewport.
        let row_bottom = request.viewport.size.height + request.virtual_viewport.document_origin.y
            - self.top
            - row_height;
        let row_left = request.virtual_viewport.layout_origin_x + owner.configuration.padding
            - request.virtual_viewport.document_origin.x;
        let frame = NSRect::new(
            NSPoint::new(row_left, row_bottom),
            NSSize::new(self.widths.iter().sum(), row_height),
        );
        let palette = owner.fonts.borrow().clone();
        let mut cells = Vec::new();
        let mut x = row_left;
        for (column, width) in self.widths.iter().copied().enumerate() {
            // All complete cell frames share actual row height; subtract original padding for text
            // boxes without accumulating guessed boundary errors.
            let cell_frame =
                NSRect::new(NSPoint::new(x, row_bottom), NSSize::new(width, row_height));
            // Subtract fixed padding from the measured row box, then subtract paged leading-line height
            // for the later text window.
            // An unfinished row represents its actual current prefix only, without full-cell height or
            // EOF authority.
            let content_frame = NSRect::new(
                NSPoint::new(
                    cell_frame.origin.x + HORIZONTAL,
                    cell_frame.origin.y + VERTICAL,
                ),
                NSSize::new(width - HORIZONTAL * 2.0, row_height - VERTICAL * 2.0),
            );
            x += width;
            if !self.column_visible(column, request, owner.configuration.padding) {
                continue;
            }
            let cell = &mut self.cells[column];
            let content = if let Some(empty) = &cell.empty {
                // Only verified raw/display emptiness uses Empty, without cell-level font or anonymous
                // mapping descriptor ownership.
                GridCellContent::Empty(empty.clone())
            } else if before_text {
                // Publish original row-top padding semantics only; actual raw.start supports cell
                // clicks,
                // while text boxes/links await content-area entry and original display mapping plus
                // actual line directories.
                let source = cell
                    .display
                    .as_ref()
                    .expect("行顶内边距发布前已取得真实格显示票")
                    .canonical_raw_cell()
                    .clone();
                #[cfg(feature = "native-markdown-runtime-probe")]
                eprintln!(
                    "native_cell_before_text column={column} row_top={} text_top={} viewport_bottom={} actual_cell_complete={}",
                    self.top,
                    self.top + VERTICAL,
                    request.virtual_viewport.document_origin.y + request.viewport.size.height,
                    cell.complete,
                );
                GridCellContent::BeforeText(source)
            } else {
                let scan = cell.scan.take().expect("所有可见格已完成真实窗口扫描");
                if scan.handles.is_empty() {
                    // Only a completely measured short cell can certify a region below its text; retain
                    // original raw click authority.
                    assert!(cell.complete, "只有真实完整短格可证明当前视口无文字");
                    let source = cell
                        .display
                        .as_ref()
                        .expect("已完成格保原显示来源")
                        .canonical_raw_cell()
                        .clone();
                    cells.push(GridCellPublication {
                        frame: cell_frame,
                        content_frame,
                        content: GridCellContent::OutsideText(source),
                    });
                    continue;
                }
                // First-line top comes from the same output-directory scan, preventing a visible suffix
                // from being drawn at the cell first-line position.
                let window_frame = NSRect::new(
                    content_frame.origin,
                    NSSize::new(
                        content_frame.size.width,
                        content_frame.size.height - scan.first_top,
                    ),
                );
                // The window borrows this Source/output/palette only, without another Body or Input
                // ownership graph for the same cell.
                let window = NativeMarkdownComposedTableCell::prepare(
                    cell.output.as_ref().expect("可见格持原prefix").clone(),
                    key.clone(),
                    cell.display.as_ref().expect("原显示源仍存活").clone(),
                    palette.clone(),
                    scan.handles,
                    window_frame,
                    current,
                )
                .map_err(scene_error)?;
                GridCellContent::Composed(window)
            };
            cells.push(GridCellPublication {
                frame: cell_frame,
                content_frame,
                content,
            });
        }
        Ok(GridRowPublication {
            row: self.row.clone(),
            frame,
            complete: self.sealed_height.is_some() || self.complete(),
            cells: cells.into_boxed_slice(),
        })
    }

    /// Use the original registered font as final directory identity without inferring actual table
    /// height from its ordinary line slot.
    pub(super) fn response(
        &self,
        owner: &Owner,
    ) -> Result<omni_markdown_core::MarkdownLineTableRowResponse> {
        let fonts = owner.fonts.borrow();
        // Widths were quantized after solving; seal each width and their sum so reconstruction cannot
        // apply a different solver.
        let widths = self
            .widths
            .iter()
            .map(|width| (width * 64.0).round() as i64)
            .collect::<Vec<_>>();
        Ok(self
            .request
            .as_ref()
            .expect("只有在途原行可归还最终几何")
            .clone()
            .respond_registered(
                fonts[0].registered(),
                (self.height() * 64.0).round() as i64,
                widths.iter().sum(),
                &widths,
            )?)
    }
}

impl Cell {
    /// Visible cells and the unique restored word-query cell observe the same actual resources without
    /// retaining Source, descriptors, or historical directories.
    fn resource_receipt(&self) -> Result<super::super::NativeMarkdownPagedCellResourceReceipts> {
        Ok(super::super::NativeMarkdownPagedCellResourceReceipts {
            display: self
                .display
                .as_ref()
                .map(|display| display.resource_observer())
                .or_else(|| {
                    self.preparation
                        .as_ref()
                        .and_then(MarkdownTableCellDisplayPreparation::resource_observer)
                }),
            writer: self
                .composer
                .as_ref()
                .map(MarkdownLineComposer::resource_observer)
                .transpose()?,
            reader: self
                .output
                .as_ref()
                .map(|output| output.borrow().resource_observer()),
            worker: self
                .composer
                .as_ref()
                .map(MarkdownLineComposer::worker_resource_observer),
        })
    }

    /// Empty fields mean actual work has not started, without default dependencies, default text, or
    /// another initialization graph.
    fn new(column: usize, width: f64) -> Self {
        Self {
            column,
            preparation: None,
            display: None,
            empty: None,
            composer: None,
            cancellation: MarkdownLineComposeCancellation::new(),
            output: None,
            pending: None,
            height: 0.0,
            lines: 0,
            complete: false,
            width,
            natural_line: 0.0,
            natural_maximum: 0.0,
            scan: None,
            target: None,
        }
    }

    /// Each turn performs one original 4 KiB semantic/conversion quantum, one Composer quantum, or
    /// bounded Native part measurement.
    fn advance(
        &mut self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        row: &omni_markdown_core::VerifiedMarkdownLayoutTableRow,
        natural: bool,
        semantics: &mut super::super::semantics::RootLeaf,
        work: &mut Work,
    ) -> Result<Advance> {
        if self.display.is_none() && self.empty.is_none() {
            let document = owner
                .document
                .borrow()
                .as_ref()
                .expect("格准备持原Body文档")
                .clone();
            let document = document.borrow();
            if self.preparation.is_none() {
                // Acquire a preparation ticket for the same sealed row only; actual semantic scanning
                // remains under the original Request allowance.
                self.preparation = semantics.prepare_cell(&document, source, row, self.column)?;
                return Ok(Advance::Progress);
            }
            let preparation = self.preparation.as_mut().expect("原格准备已安装");
            let before = semantics.current_work();
            let step = semantics.advance_cell(preparation, &document)?;
            // Returned work includes CellInline; subtract same-turn work already accounted by the
            // original Request before adding outer display conversion.
            let measured = match &step {
                MarkdownTableCellDisplayPreparationStep::Progress { work }
                | MarkdownTableCellDisplayPreparationStep::Ready { work } => *work,
            };
            work.display_step(measured, before, semantics.current_work())?;
            match step {
                MarkdownTableCellDisplayPreparationStep::Progress { .. } => {
                    // Incomplete conversion grants no display authority, default style, or Composer
                    // admission.
                    return Ok(Advance::Progress);
                }
                MarkdownTableCellDisplayPreparationStep::Ready { .. } => {
                    // Only the original ready factory yields display Source, held through
                    // mappings/paged directories rather than a whole-cell String.
                    let display = preparation
                        .take_ready_for(&document)?
                        .expect("Ready持原显示源");
                    if display.source().utf16_len() == 0
                        && display
                            .canonical_raw_cell()
                            .canonical_range_for(&document)?
                            .is_empty()
                    {
                        // Hidden syntax can empty display without emptying raw text; preserve edit
                        // coordinates of nonempty cells.
                        self.empty = Some(display.canonical_raw_cell().clone());
                        self.complete = true;
                    } else {
                        self.display = Some(Rc::new(display));
                    }
                    self.preparation = None;
                    return Ok(Advance::Progress);
                }
            }
        }
        if self.complete {
            return Ok(Advance::Complete);
        }
        if self.composer.is_none() {
            let display = self.display.as_ref().expect("非空格持原显示源");
            let content = if natural {
                // Initial natural-width measurement uses actual advances and stops at the original cap,
                // without shaping huge cells to EOF.
                self.width
            } else {
                native_markdown_table_content_width(self.width).ok_or(ViewError::InvalidGeometry)?
            };
            // All columns share the body catalog; each cell holds its own cancellation handle without a
            // per-cell FontWorker or Input.
            self.composer = Some(MarkdownLineComposer::new_with_frontier(
                display.source().clone(),
                &display.authority(),
                owner.catalog.clone(),
                (content * 64.0).round() as i64,
                self.cancellation.clone(),
            )?);
            return Ok(Advance::Progress);
        }
        if let Some(mut metrics) = self.pending.take() {
            let mut budget = LINE_METRIC_BUDGET;
            let output = self.output.as_ref().expect("待度量行持原prefix Reader");
            if !metrics.advance(output, owner, &mut budget, work)? {
                // Retain continuation until every actual part metric completes; a partial line height
                // cannot enter the real prefix.
                self.pending = Some(metrics);
                return Ok(Advance::Progress);
            }
            // Grow the prefix only with complete metrics and discard old scans to avoid stale
            // bottom/Reader reuse.
            self.height += metrics.height()?;
            self.lines += 1;
            self.scan = None;
            return Ok(Advance::Progress);
        }
        let composer = self.composer.as_mut().expect("活格持原Composer");
        let before = composer.frontier_work()?;
        // Use fixed per-turn read/scalar/directory budgets, without enlarging them for cell length or
        // reading the whole paragraph first.
        let step = composer.step(MarkdownLineComposeBudget {
            bytes: 4096,
            scalars: 256,
            records: 256,
            metadata_units: 256,
        })?;
        work.composer_step(&step, before, composer.frontier_work()?)?;
        match step.answer {
            MarkdownLineComposeAnswer::Yielded => Ok(Advance::Progress),
            MarkdownLineComposeAnswer::NeedStyle(request) => Ok(Advance::Style {
                column: self.column,
                request,
            }),
            MarkdownLineComposeAnswer::NeedsControl(request) => Ok(Advance::Control {
                column: self.column,
                request,
            }),
            MarkdownLineComposeAnswer::LineReady(facts) => {
                if let Some(target) = self.target.as_mut() {
                    target.reset_scan();
                }
                if natural {
                    // Soft-wrap advances remain in the same physical line; reset natural line width
                    // only at an actual mandatory break.
                    self.natural_line += facts.x_advance_26_6 as f64 / 64.0;
                    self.natural_maximum = self.natural_maximum.max(self.natural_line);
                    if facts.kind == omni_markdown_core::MarkdownComposedLineKind::Mandatory {
                        self.natural_line = 0.0;
                    }
                } else {
                    // A new prefix replaces the old Reader; any Scene retaining its old window releases
                    // it through that actual lease.
                    drop(self.output.take());
                    let output = Rc::new(RefCell::new(
                        self.composer
                            .as_mut()
                            .expect("LineReady仍属原Composer")
                            .publish_ready_prefix()?
                            .expect("完整LineEnd可发布真实prefix"),
                    ));
                    let handle = output.borrow_mut().last_line()?.expect("LineReady持原末行");
                    if handle.facts() != &facts || facts.ordinal != self.lines {
                        // Mismatched facts/ordinals can cause wrong heights and overlapping text;
                        // propagate actual failure upward.
                        #[cfg(feature = "native-markdown-runtime-probe")]
                        eprintln!(
                            "native_cell_unsupported phase=line_identity column={} expected_line={} actual_line={} original_facts_match={}",
                            self.column,
                            self.lines,
                            facts.ordinal,
                            handle.facts() == &facts,
                        );
                        return Err(Error::Unsupported);
                    }
                    self.pending = Some(LineMetrics::new(
                        handle,
                        omni_markdown_core::MarkdownPresentationMode::InlinePreview,
                        false,
                    ));
                    self.output = Some(output);
                }
                Ok(Advance::Progress)
            }
            MarkdownLineComposeAnswer::Complete => {
                if let Some(target) = self.target.as_mut() {
                    target.reset_scan();
                }
                self.complete = true;
                if natural {
                    // Natural-width output is not painted; cancel and return its Source, Reader, and
                    // directory resources on completion.
                    self.release_layout();
                } else {
                    // Acquire final output only at Complete; an ordinary prefix certifies neither
                    // full-cell EOF nor final row maximum.
                    drop(self.output.take());
                    self.output = Some(Rc::new(RefCell::new(
                        self.composer
                            .take()
                            .expect("完成时持原Composer")
                            .into_output()?,
                    )));
                }
                Ok(Advance::Progress)
            }
            answer => {
                // The gated synthetic diagnostic records actual unsupported Core answers without cell
                // text/links or changed failure semantics.
                #[cfg(feature = "native-markdown-runtime-probe")]
                match answer {
                    MarkdownLineComposeAnswer::NeedsResolution(reason) => eprintln!(
                        "native_cell_unsupported phase=resolution column={} reason={reason:?}",
                        self.column,
                    ),
                    MarkdownLineComposeAnswer::NeedsFrontier(reason) => eprintln!(
                        "native_cell_unsupported phase=frontier column={} reason={reason:?}",
                        self.column,
                    ),
                    MarkdownLineComposeAnswer::NeedsTabPlacement(reason) => eprintln!(
                        "native_cell_unsupported phase=tab column={} reason={reason:?}",
                        self.column,
                    ),
                    MarkdownLineComposeAnswer::NeedsTableRow(_) => eprintln!(
                        "native_cell_unsupported phase=nested_table_row column={}",
                        self.column,
                    ),
                    _ => unreachable!("其余真实 Core 回答已在上方处理"),
                }
                #[cfg(not(feature = "native-markdown-runtime-probe"))]
                let _ = answer;
                Err(Error::Unsupported) // Unresolved Unicode/Tab/frontier work cannot be truncated into fabricated EOF.
            }
        }
    }

    /// At natural-width cap or actual retirement, cancel and release owned layout/Source immediately
    /// without changing canonical content.
    fn release_layout(&mut self) {
        self.cancellation.cancel();
        self.pending = None;
        self.scan = None;
        if let Some(target) = self.target.as_mut() {
            target.reset_scan();
        }
        drop(self.output.take());
        drop(self.composer.take());
        drop(self.display.take());
        drop(self.preparation.take());
    }

    /// Walk backward from the original last line to the viewport top, leaving offscreen glyphs in paged
    /// directories without whole-cell line arrays.
    fn scan_window(
        &mut self,
        owner: &Owner,
        request: &NativeMarkdownPagedSceneRequest,
        row_top: f64,
        budget: &mut MarkdownLineComposeBudget,
        work: &mut Work,
    ) -> Result<bool> {
        if self
            .scan
            .as_ref()
            .is_some_and(|scan| !scan.request.same(request))
        {
            // Scroll/size/selection epoch changes invalidate the old viewport cursor; retain no stale
            // visible window.
            self.scan = None;
        }
        let output = self.output.as_ref().expect("扫描只接受真实已封存prefix");
        if self.scan.is_none() {
            // Begin paged backward scanning at the actual last line; handles contain the intersecting
            // window only, without historical-line copies.
            let cursor = output.borrow_mut().last_line()?;
            self.scan = Some(CellScan {
                request: request.clone(),
                cursor,
                metrics: None,
                bottom: self.height,
                first_top: 0.0,
                handles: VecDeque::new(),
                ready: false,
            });
        }
        let align = {
            let original = owner
                .document
                .borrow()
                .as_ref()
                .expect("原显示格沿父Doc")
                .clone();
            let document = original.borrow();
            self.display
                .as_ref()
                .expect("可见格持原显示Source")
                .canonical_raw_cell()
                .alignment_for(&document)?
                != omni_markdown_core::MarkdownTableAlignment::Left
        };
        let scan = self.scan.as_mut().expect("原扫描已安装");
        if scan.ready {
            return Ok(true);
        }
        let top = (request.virtual_viewport.document_origin.y - row_top - VERTICAL).max(0.0);
        let bottom = request.virtual_viewport.document_origin.y + request.viewport.size.height
            - row_top
            - VERTICAL;
        for _ in 0..METADATA_PER_TURN {
            if budget.metadata_units == 0 || budget.records < 7 {
                // Visible rows share this allowance; retain the original cursor for later turns rather
                // than rescanning the first cell.
                return Ok(false);
            }
            if scan.metrics.is_none() {
                let Some(handle) = scan.cursor.take() else {
                    break;
                };
                scan.metrics = Some(LineMetrics::new(
                    handle,
                    omni_markdown_core::MarkdownPresentationMode::InlinePreview,
                    align,
                ));
            }
            let mut metrics = scan.metrics.take().expect("本回合持原行度量");
            if !metrics.advance(output, owner, budget, work)? {
                // Mixed-font parts may span turns; do not subtract bottom or publish line geometry
                // until they finish.
                scan.metrics = Some(metrics);
                return Ok(false);
            }
            let height = metrics.height()?;
            let line_top = scan.bottom - height;
            let handle = metrics.into_measured()?;
            let previous = output.borrow_mut().previous_line(handle.handle())?;
            if scan.bottom >= top && line_top <= bottom {
                if scan.handles.len() == MAX_WINDOW_LINES {
                    // Exceeding bounded window-handle capacity returns an actual error instead of
                    // truncating rendering into a supposedly complete frame.
                    return Err(Error::Unsupported);
                }
                scan.first_top = line_top;
                scan.handles.push_front(handle);
            }
            scan.bottom = line_top;
            if line_top <= top {
                // After crossing the actual viewport top, leave earlier lines in paged directories
                // rather than scanning to cell start.
                scan.cursor = None;
                break;
            }
            scan.cursor = previous;
        }
        scan.ready = scan.cursor.is_none() && scan.metrics.is_none();
        if scan.ready && scan.handles.is_empty() && !self.complete {
            // An empty prefix of an unfinished cell cannot prove text absence; OutsideText must not
            // hide missing content.
            #[cfg(feature = "native-markdown-runtime-probe")]
            eprintln!(
                "native_cell_unsupported phase=uncovered_window column={} row_top={row_top} text_window_top={top} text_window_bottom={bottom} actual_prefix_height={} actual_prefix_lines={}",
                self.column, self.height, self.lines,
            );
            return Err(Error::Unsupported);
        }
        Ok(scan.ready)
    }
}
impl Drop for Cell {
    /// On exit cancel original composition tasks; actual last field owners release Readers and Source.
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

impl Work {
    /// Trailing-whitespace queries return actual Source/grapheme-directory work without claiming shaper
    /// input bytes.
    pub(super) fn alignment_step(
        &mut self,
        step: &omni_markdown_core::MarkdownComposedLineTrailingWhitespaceStep,
    ) -> Result<()> {
        for (total, value) in [
            (&mut self.bytes, step.bytes),
            (&mut self.scalars, step.scalars),
            (&mut self.metadata, step.metadata_units),
        ] {
            *total = total.checked_add(value).ok_or(Error::CounterExhausted)?;
        }
        self.maximum_bytes = self.maximum_bytes.max(step.bytes);
        self.maximum_scalars = self.maximum_scalars.max(step.scalars);
        self.maximum_metadata = self.maximum_metadata.max(step.metadata_units);
        Ok(())
    }

    /// Subtract same-quantum original Request CellInline deltas so outer totals receive actual
    /// display-conversion work only.
    fn display_step(
        &mut self,
        measured: SemanticWork,
        before: SemanticWork,
        after: SemanticWork,
    ) -> Result<()> {
        let remainder = |measured: usize, before: usize, after: usize| {
            // Decreasing totals or duplicate work above this turn measurement would corrupt resource
            // evidence and therefore fail accounting.
            after
                .checked_sub(before)
                .and_then(|owned| measured.checked_sub(owned))
                .ok_or(Error::CounterExhausted)
        };
        let display = SemanticWork {
            source_bytes: remainder(
                measured.source_bytes,
                before.source_bytes,
                after.source_bytes,
            )?,
            copied_source_bytes: remainder(
                measured.copied_source_bytes,
                before.copied_source_bytes,
                after.copied_source_bytes,
            )?,
            metadata_bytes: remainder(
                measured.metadata_bytes,
                before.metadata_bytes,
                after.metadata_bytes,
            )?,
            utf8_validation_bytes: remainder(
                measured.utf8_validation_bytes,
                before.utf8_validation_bytes,
                after.utf8_validation_bytes,
            )?,
            source_tree_steps: remainder(
                measured.source_tree_steps,
                before.source_tree_steps,
                after.source_tree_steps,
            )?,
            structural_transitions: remainder(
                measured.structural_transitions,
                before.structural_transitions,
                after.structural_transitions,
            )?,
            allocated_source_capacity: remainder(
                measured.allocated_source_capacity,
                before.allocated_source_capacity,
                after.allocated_source_capacity,
            )?,
            normalizer_input_bytes: remainder(
                measured.normalizer_input_bytes,
                before.normalizer_input_bytes,
                after.normalizer_input_bytes,
            )?,
            parser_input_bytes: remainder(
                measured.parser_input_bytes,
                before.parser_input_bytes,
                after.parser_input_bytes,
            )?,
        };
        add_semantic_work(&mut self.display, display)
    }

    /// Keep actual Composer step reads, maxima, and frontier deltas rather than estimating cost from
    /// character length.
    fn composer_step(
        &mut self,
        step: &omni_markdown_core::MarkdownLineComposeStep,
        before: omni_markdown_core::MarkdownLineFrontierWork,
        after: omni_markdown_core::MarkdownLineFrontierWork,
    ) -> Result<()> {
        let frontier = after
            .source_bytes
            .checked_sub(before.source_bytes)
            .ok_or(Error::CounterExhausted)?;
        let shaped = after
            .input_bytes
            .checked_sub(before.input_bytes)
            .ok_or(Error::CounterExhausted)?;
        for (total, value) in [
            (&mut self.bytes, step.bytes),
            (&mut self.scalars, step.scalars),
            (&mut self.metadata, step.metadata_units),
            (&mut self.frontier_source_bytes, frontier),
            (&mut self.shaped_input_bytes, shaped),
        ] {
            // Add original Composer facts only; overflow must not saturate into a deceptively valid
            // small measurement.
            *total = total.checked_add(value).ok_or(Error::CounterExhausted)?;
        }
        self.maximum_bytes = self.maximum_bytes.max(step.bytes);
        self.maximum_scalars = self.maximum_scalars.max(step.scalars);
        self.maximum_metadata = self.maximum_metadata.max(step.metadata_units);
        Ok(())
    }
}
