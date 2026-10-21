//! A bounded table-structure request over actual source pieces. Its internal
//! structural receipt does not stand in for the later complete semantic Ready.

use super::table_source::MarkdownTableSourceIdentity;
use super::*;
use crate::markdown_viewport_presentation::table_window::{
    MarkdownTableWindowError, TABLE_WINDOW_MAX_CELLS, TABLE_WINDOW_MAX_COLUMNS,
    TABLE_WINDOW_MAX_ROWS,
};
mod cell_edit;
pub use cell_edit::{MarkdownTableCellEditRequest, MarkdownTableCellEditStep};
mod checkpoint;
use checkpoint::{Boundary, ReplayAnchors};
pub(in crate::markdown_document_buffer) mod ordinary_source;
use ordinary_source::CapturedOrdinary;
#[cfg(feature = "paged-line-compose")]
pub use ordinary_source::MarkdownSelectionStyleInvariant;
pub use ordinary_source::{
    MarkdownOrdinaryPendingReason, MarkdownOrdinarySemanticState, MarkdownRootLeafPendingReason,
    MarkdownTableCellInlineStep, MarkdownViewportSourceAuthority, MarkdownViewportSourceRequest,
    MarkdownViewportSourceRequiredRowQuery, MarkdownViewportSourceRequiredRowResult,
    MarkdownViewportSourceStep, VerifiedMarkdownOrdinaryLeaf, VerifiedMarkdownOrdinaryLine,
    VerifiedMarkdownPlainViewport, VerifiedMarkdownRootLeafViewport,
    VerifiedMarkdownTableCellInline, VerifiedMarkdownTableRowSource,
    VerifiedMarkdownViewportSources,
};

use pulldown_cmark::{
    block_frontier::{SourceFrame, TableCheckpoint, TableRowFact, VerifiedTableInlineRow},
    global_frontier::{
        GlobalFact, GlobalFrontier, GlobalFrontierError, GlobalFrontierLimits,
        GlobalSourceAuthority, GlobalStep, GlobalTableFact, GlobalWork, StructuralSpineLimits,
        VerifiedTableEnd,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Viewport lookup uses actual UTF-16 positions; row navigation uses physical
/// header identity and an absolute row ordinal.
pub enum MarkdownTableWindowTarget {
    AtUtf16(usize),
    /// Find the first actual table intersecting the viewport. Ordinary text at
    /// its top does not prove that no table exists farther down the same viewport.
    FirstTableInViewport,
    RequiredRow {
        header_byte_start: usize,
        row_ordinal: usize,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownTableRequestError {
    /// Actual original Source read/reclaim failure, preserved by its owning public
    /// boundary rather than reclassified as cancellation or structural coordinates.
    Source(MarkdownDocumentBufferError),
    /// Actual first I/O failure of this Root inline job's anonymous matching stack.
    /// Do not return partial styles or reinterpret a short read as an empty index.
    InlineStorageUnavailable(i32),
    InvalidRequest,
    SourceSuperseded,
    /// Continuation requires this request's last opaque publication, not another
    /// same-generation batch or an earlier publication.
    PublicationSuperseded,
    /// Required left/interior coverage lacks an actual checkpoint. The caller
    /// cannot silently restart from source zero and pretend it continued existing coverage.
    CoverageRequiresCheckpoint,
    Cancelled,
    WorkLimit,
    Global(GlobalFrontierError),
    Window(MarkdownTableWindowError),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
/// Accumulate actual replay reads and state transitions. Source cursor distance
/// is not a substitute for the work already performed.
pub(crate) struct MarkdownTableRequestWork {
    pub byte_reads: usize,
    pub transitions: usize,
    pub source_tree_steps: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MarkdownTableStructureStep {
    Progress { work: MarkdownTableRequestWork },
    StructureReady { work: MarkdownTableRequestWork },
    NotTable { work: MarkdownTableRequestWork },
    TableEndReached { work: MarkdownTableRequestWork },
}

#[derive(Debug)]
/// Only the actual GlobalFact consumer can construct this receipt; fields and
/// conversion constructors remain private. It proves membership/coordinates only,
/// not references, underline or inherited styles, and cannot authorize Native to
/// hide source text directly.
pub(crate) struct MarkdownTableStructuralWindow {
    source: MarkdownTableSourceIdentity,
    header: GlobalTableFact,
    checkpoint: TableCheckpoint,
    header_utf16: Range<usize>,
    delimiter_utf16: Range<usize>,
    body: Vec<TableRowFact>,
    inline_rows: Vec<VerifiedTableInlineRow>,
    include_header: bool,
    next_required_row: Option<usize>,
    table_end: Option<VerifiedTableEnd>,
    replay: Option<ReplayAnchors>,
}

impl MarkdownTableStructuralWindow {
    /// Semantic admission still checks the original root/generation. Equal
    /// integer coordinates cannot replace the structural receipt's Source identity.
    fn is_current(&self, document: &MarkdownDocumentBuffer) -> bool {
        self.source.is_current(document)
    }
}

#[cfg(test)]
impl MarkdownTableStructuralWindow {
    pub fn rows(&self) -> &[TableRowFact] {
        &self.body
    }
    pub fn includes_header(&self) -> bool {
        self.include_header
    }
    pub fn next_required_row(&self) -> Option<usize> {
        self.next_required_row
    }
    pub fn table_end(&self) -> Option<&VerifiedTableEnd> {
        self.table_end.as_ref()
    }
}

struct ActiveTable {
    header: GlobalTableFact,
    checkpoint: TableCheckpoint,
    header_utf16: Range<usize>,
    delimiter_utf16: Range<usize>,
    body: Vec<TableRowFact>,
    inline_rows: Vec<VerifiedTableInlineRow>,
    found: bool,
    include_header: bool,
    first_overflow: Option<usize>,
    // Retain the actual delimiter boundary, current accepted boundary and its
    // predecessor, without building a checkpoint directory for every past row.
    base_boundary: Option<Boundary>,
    boundary: Option<Boundary>,
    halo_boundary: Option<Boundary>,
    chunk_boundary: Option<Boundary>,
    chunk_halo: Option<Boundary>,
}

/// The request retains no document, mapping or text String. Each advance borrows
/// one actual source frame from its current owner. Pending, structural completion
/// and absence are distinct; sticky failure cannot resume toward a false Complete.
pub(crate) struct MarkdownTableWindowStructureRequest {
    source: MarkdownTableSourceIdentity,
    target: MarkdownTableWindowTarget,
    continuous: bool,
    continuous_body_limit: Option<usize>,
    viewport: Range<usize>,
    global: Option<GlobalFrontier>,
    pending: Option<GlobalFact>,
    // Enabled only by the source-aware wrapper. Table-only consumers deliberately
    // ignore ordinary-block facts rather than create an unrelated source inventory.
    capture_ordinary: bool,
    ordinary_pending: Option<CapturedOrdinary>,
    coordinates: Option<HeaderCoordinates>,
    current: Option<ActiveTable>,
    ready: Option<MarkdownTableStructuralWindow>,
    no_table: bool,
    required_row_end: Option<VerifiedTableEnd>,
    failed: Option<MarkdownTableRequestError>,
    total: MarkdownTableRequestWork,
    max_byte_work: usize,
    max_transitions: usize,
}

impl MarkdownDocumentBuffer {
    /// Bind this request to the current nonempty piece root. Target/viewport are
    /// supplied actual coordinates, without probing other personal files.
    pub(crate) fn table_window_structure_request(
        &self,
        target: MarkdownTableWindowTarget,
        viewport: Range<usize>,
    ) -> Result<MarkdownTableWindowStructureRequest, MarkdownTableRequestError> {
        let source = self
            .table_source_identity()
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        if viewport.start > viewport.end
            || viewport.end > self.utf16_len()
            || viewport.end - viewport.start > 64 * 1024
            || matches!(target, MarkdownTableWindowTarget::AtUtf16(at) if !viewport.contains(&at))
            || matches!(target, MarkdownTableWindowTarget::FirstTableInViewport)
                && viewport.is_empty()
        {
            // A caller cannot widen the bounded viewport or turn an out-of-range
            // EOF caret into evidence of membership in an actual source table.
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        let max_byte_work = self
            .byte_len()
            .checked_mul(256)
            .and_then(|n| n.checked_add(1024 * 1024))
            .ok_or(MarkdownTableRequestError::WorkLimit)?;
        let max_transitions = self
            .byte_len()
            .checked_mul(2048)
            .and_then(|n| n.checked_add(1024 * 1024))
            .ok_or(MarkdownTableRequestError::WorkLimit)?;
        let options =
            crate::markdown_viewport_presentation::parser::markdown_presentation_options(true);
        let global = GlobalFrontier::new(
            source.generation(),
            source.byte_len(),
            options,
            GlobalFrontierLimits {
                spine: StructuralSpineLimits {
                    max_depth: 16_384,
                    max_live_page_bytes: 4 * 1024 * 1024,
                    max_page_allocations: 1_000_000,
                    max_role_work: max_transitions,
                },
                max_byte_work,
                max_transitions,
            },
        )
        .map_err(MarkdownTableRequestError::Global)?;
        Ok(MarkdownTableWindowStructureRequest {
            source,
            target,
            continuous: false,
            continuous_body_limit: None,
            viewport,
            global: Some(global),
            pending: None,
            capture_ordinary: false,
            ordinary_pending: None,
            coordinates: None,
            current: None,
            ready: None,
            no_table: false,
            required_row_end: None,
            failed: None,
            total: MarkdownTableRequestWork::default(),
            max_byte_work,
            max_transitions,
        })
    }
}

impl MarkdownTableWindowStructureRequest {
    #[cfg(test)]
    pub(crate) fn total_work(&self) -> MarkdownTableRequestWork {
        self.total
    }

    /// Keep per-step byte/transition work within the caller's allowance. Consume
    /// actual structure facts on the next step so a Scanner that used all its
    /// fuel is not followed by hidden coordinate work or allocation. Source leases
    /// end within this Rust call and never cross a native callback.
    pub(crate) fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        cancelled: bool,
        budget: usize,
    ) -> Result<MarkdownTableStructureStep, MarkdownTableRequestError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        let result = self.advance_inner(document, cancelled, budget);
        match result {
            Ok(step) => {
                let work = match step {
                    MarkdownTableStructureStep::Progress { work }
                    | MarkdownTableStructureStep::StructureReady { work }
                    | MarkdownTableStructureStep::NotTable { work }
                    | MarkdownTableStructureStep::TableEndReached { work } => work,
                };
                let cumulative = self
                    .total
                    .byte_reads
                    .checked_add(work.byte_reads)
                    .zip(
                        self.total
                            .source_tree_steps
                            .checked_add(work.source_tree_steps),
                    )
                    .zip(self.total.transitions.checked_add(work.transitions));
                let Some(((bytes, tree), transitions)) = cumulative else {
                    self.fail(MarkdownTableRequestError::WorkLimit);
                    return Err(MarkdownTableRequestError::WorkLimit);
                };
                self.total = MarkdownTableRequestWork {
                    byte_reads: bytes,
                    source_tree_steps: tree,
                    transitions,
                };
                if self.total.byte_reads > self.max_byte_work
                    || self.total.transitions > self.max_transitions
                {
                    // Metadata and spine work share this request's lifetime fuel;
                    // entering another structural phase does not reset its totals.
                    self.fail(MarkdownTableRequestError::WorkLimit);
                    return Err(MarkdownTableRequestError::WorkLimit);
                }
                Ok(step)
            }
            Err(error) => {
                self.fail(error);
                Err(error)
            }
        }
    }

    fn fail(&mut self, error: MarkdownTableRequestError) {
        // Revoke on the first failure and drop derived metadata/shared spine and
        // their capacities. Source retains only weak identity; canonical text and
        // history stay untouched, and stale Ready cannot escape through getters.
        self.failed = Some(error);
        self.global = None;
        self.pending = None;
        self.ordinary_pending = None;
        self.coordinates = None;
        self.current = None;
        self.ready = None;
        self.required_row_end = None;
    }

    fn advance_inner(
        &mut self,
        document: &MarkdownDocumentBuffer,
        cancelled: bool,
        budget: usize,
    ) -> Result<MarkdownTableStructureStep, MarkdownTableRequestError> {
        if cancelled {
            return Err(MarkdownTableRequestError::Cancelled);
        }
        if !self.source.is_current(document) {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        let minimum = if self.continuous { 3072 } else { 4096 };
        if !(minimum..=64 * 1024).contains(&budget) {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        if self.ready.is_some() {
            return Ok(MarkdownTableStructureStep::StructureReady {
                work: MarkdownTableRequestWork::default(),
            });
        }
        if self.required_row_end.is_some() {
            return Ok(MarkdownTableStructureStep::TableEndReached {
                work: MarkdownTableRequestWork::default(),
            });
        }
        if self.no_table {
            return Ok(MarkdownTableStructureStep::NotTable {
                work: MarkdownTableRequestWork::default(),
            });
        }
        if self.ordinary_pending.is_some() {
            // Pause this same Global until its original receipt is transferred.
            // Do not overwrite pending or silently scan another ordinary block.
            return Ok(MarkdownTableStructureStep::Progress {
                work: MarkdownTableRequestWork::default(),
            });
        }
        if self.coordinates.is_some() {
            return self.advance_coordinates(document, budget);
        }
        if let Some(fact) = self.pending.take() {
            // One fact carries bounded metadata. Consuming it does not reread a
            // source row whose structural scan has already finished.
            return self.consume_fact(document, fact, budget);
        }
        let source_tree_steps = tree_height(document);
        let child_budget = child_budget(budget, source_tree_steps)?;
        let global = self
            .global
            .as_mut()
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        let frame = document
            .table_source_frame(global.next_byte(), child_budget)
            .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
        let step = global
            .advance(
                SourceFrame {
                    source_revision: frame.generation,
                    byte_start: frame.byte_start,
                    bytes: &frame.bytes,
                    real_eof: frame.real_eof,
                },
                GlobalSourceAuthority {
                    current_revision: document.generation(),
                    cancelled: false,
                },
                child_budget,
            )
            .map_err(MarkdownTableRequestError::Global);
        // Consume the Parser result only after the actual frame returns. Early
        // Parser completion cannot turn a reclaim failure into structural success.
        frame
            .finish()
            .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
        let step = step?;
        let work = match step {
            GlobalStep::Progress { work, .. } | GlobalStep::NeedInput { work, .. } => work,
            GlobalStep::Fact { fact, work } => {
                self.pending = Some(fact);
                work
            }
            GlobalStep::Complete { work } => {
                self.no_table = true;
                if !self.continuous {
                    self.global = None;
                }
                self.current = None;
                return Ok(MarkdownTableStructureStep::NotTable {
                    work: global_work(work, source_tree_steps),
                });
            }
        };
        Ok(MarkdownTableStructureStep::Progress {
            work: global_work(work, source_tree_steps),
        })
    }

    fn consume_fact(
        &mut self,
        document: &MarkdownDocumentBuffer,
        fact: GlobalFact,
        budget: usize,
    ) -> Result<MarkdownTableStructureStep, MarkdownTableRequestError> {
        let work = MarkdownTableRequestWork {
            byte_reads: 0,
            transitions: 64,
            source_tree_steps: 0,
        };
        match fact {
            GlobalFact::Table(header) => {
                // Locate one actual boundary leaf now; measure header/delimiter
                // UTF-16 in later bounded steps. Avoid the general offset lookup
                // repeatedly decoding a potentially 64 KiB edited leaf prefix.
                let coordinates = HeaderCoordinates::new(document, header, budget)?;
                let source_tree_steps = tree_height(document);
                self.coordinates = Some(coordinates);
                return Ok(MarkdownTableStructureStep::Progress {
                    work: MarkdownTableRequestWork {
                        byte_reads: 0,
                        transitions: source_tree_steps + 64,
                        source_tree_steps,
                    },
                });
            }
            GlobalFact::Row(row) => {
                let current = self
                    .current
                    .as_mut()
                    .ok_or(MarkdownTableRequestError::InvalidRequest)?;
                if row.header_byte_start != current.header.header_byte_start {
                    return Err(MarkdownTableRequestError::InvalidRequest);
                }
                let target_here = match self.target {
                    MarkdownTableWindowTarget::AtUtf16(at) => {
                        (row.utf16_start..row.utf16_end).contains(&at)
                    }
                    MarkdownTableWindowTarget::FirstTableInViewport => {
                        row.utf16_start < self.viewport.end && self.viewport.start < row.utf16_end
                    }
                    MarkdownTableWindowTarget::RequiredRow {
                        header_byte_start,
                        row_ordinal,
                    } => {
                        header_byte_start == row.header_byte_start && row_ordinal == row.row_ordinal
                    }
                };
                if target_here {
                    current.found = true;
                    validate_columns(current.header.columns)?;
                }
                let selected = match self.target {
                    MarkdownTableWindowTarget::AtUtf16(_)
                    | MarkdownTableWindowTarget::FirstTableInViewport => {
                        row.utf16_start < self.viewport.end && self.viewport.start < row.utf16_end
                    }
                    MarkdownTableWindowTarget::RequiredRow {
                        header_byte_start,
                        row_ordinal,
                    } => {
                        header_byte_start == row.header_byte_start && row.row_ordinal == row_ordinal
                    }
                };
                if current.found && current.first_overflow.is_some() {
                    let next = current.first_overflow;
                    if self.continuous {
                        self.pending = Some(GlobalFact::Row(row));
                    }
                    self.finish(next, None)?;
                    return Ok(MarkdownTableStructureStep::StructureReady { work });
                }
                if current.found && !selected {
                    // Coverage has passed this window: read no extra row body,
                    // and do not promote its local last row into the table's End.
                    if self.continuous {
                        self.pending = Some(GlobalFact::Row(row));
                    }
                    self.finish(None, None)?;
                    return Ok(MarkdownTableStructureStep::StructureReady { work });
                }
                if selected {
                    let maximum_body = (TABLE_WINDOW_MAX_CELLS / current.header.columns)
                        .saturating_sub(1)
                        .min(TABLE_WINDOW_MAX_ROWS - 1)
                        .min(self.continuous_body_limit.unwrap_or(usize::MAX));
                    if maximum_body == 0 && self.continuous {
                        return Err(MarkdownTableRequestError::Window(
                            MarkdownTableWindowError::RowLimit,
                        ));
                    }
                    if current.body.len() == maximum_body {
                        // Target may lie inside the viewport. Even when the first
                        // chunk fills earlier, prove same-table target membership
                        // without dropping the already-visible preceding rows.
                        current.first_overflow.get_or_insert(row.row_ordinal);
                        if current.found {
                            if self.continuous {
                                self.pending = Some(GlobalFact::Row(row));
                            }
                            self.finish(Some(row.row_ordinal), None)?;
                            return Ok(MarkdownTableStructureStep::StructureReady { work });
                        }
                    }
                    if current.first_overflow.is_none() {
                        if current.body.is_empty() {
                            // Save this row's actual predecessor for later replay;
                            // viewport positions cannot manufacture a checkpoint.
                            current.chunk_boundary = current.boundary.clone();
                            current.chunk_halo = current.halo_boundary.clone();
                        }
                        current.body.push(row);
                        // Capture the original row from the same accepted checkpoint
                        // as its Row fact, not from a window-derived TableCell range.
                        current.inline_rows.push(
                            self.global
                                .as_ref()
                                .and_then(GlobalFrontier::table_checkpoint)
                                .and_then(|checkpoint| checkpoint.last_inline_row())
                                .ok_or(MarkdownTableRequestError::InvalidRequest)?,
                        );
                    }
                }
                // This same Global has accepted the complete row/checkpoint.
                // No disabled display extension justifies another source read.
                let checkpoint = self
                    .global
                    .as_ref()
                    .and_then(GlobalFrontier::table_checkpoint)
                    .ok_or(MarkdownTableRequestError::InvalidRequest)?;
                current.halo_boundary = current.boundary.take();
                current.boundary = Some(Boundary::after_row(checkpoint, row));
            }
            GlobalFact::TableEnd(end) => {
                if self.current.as_ref().is_some_and(|table| table.found) {
                    self.finish(None, Some(end))?;
                    return Ok(MarkdownTableStructureStep::StructureReady { work });
                }
                if matches!(self.target, MarkdownTableWindowTarget::RequiredRow { header_byte_start, .. }
                    if header_byte_start == end.header_byte_start())
                {
                    // A missing successor row is distinct from not being a table.
                    // Retain the actual Ended proof; upper layers still verify
                    // final row/column before append, rather than trusting request coordinates.
                    self.required_row_end = Some(end);
                    self.current = None;
                    self.global = None;
                    return Ok(MarkdownTableStructureStep::TableEndReached { work });
                }
                // The target is outside this table. Drop its checkpoint/metadata
                // and continue the same Global, without retaining a table-history index.
                self.current = None;
            }
            GlobalFact::Reference(_) | GlobalFact::Footnote(_) => {} // Structure lookup retains no document-wide reference index; bounded queries later resolve actual source order.
            GlobalFact::InlineBlock(block) => {
                if self.capture_ordinary {
                    // Retain this same actual frontier's opaque capture. A later
                    // consumer cannot reconstruct a paragraph from a gap range.
                    self.ordinary_pending = Some(CapturedOrdinary::Inline(block));
                }
            }
            GlobalFact::LeafBlock(block) => {
                if self.capture_ordinary {
                    // Leaf has no per-line prefix permission yet. Transferring its
                    // original closed-source receipt does not create ordinary Ready.
                    self.ordinary_pending = Some(CapturedOrdinary::Leaf(block));
                }
            }
        }
        Ok(MarkdownTableStructureStep::Progress { work })
    }

    fn install_header(
        &mut self,
        header: GlobalTableFact,
        coordinates: [usize; 4],
    ) -> Result<(), MarkdownTableRequestError> {
        let checkpoint = self
            .global
            .as_ref()
            .and_then(GlobalFrontier::table_checkpoint)
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        let header_utf16 = coordinates[0]..coordinates[1];
        let delimiter_utf16 = coordinates[2]..coordinates[3];
        let found = match self.target {
            MarkdownTableWindowTarget::AtUtf16(at) => {
                (header_utf16.start..delimiter_utf16.end).contains(&at)
            }
            MarkdownTableWindowTarget::FirstTableInViewport => {
                header_utf16.start < self.viewport.end && self.viewport.start < delimiter_utf16.end
            }
            MarkdownTableWindowTarget::RequiredRow {
                header_byte_start,
                row_ordinal,
            } => header_byte_start == header.header_byte_start && row_ordinal == 0,
        };
        let include_header = header_utf16.start < self.viewport.end
            && self.viewport.start < delimiter_utf16.end
            || matches!(
                self.target,
                MarkdownTableWindowTarget::RequiredRow { row_ordinal: 0, .. }
            );
        if found {
            validate_columns(header.columns)?;
        }
        // Header/delimiter coordinates and original Global checkpoint are complete;
        // establish the actual replay entrance for the first body row directly.
        let boundary = Boundary::after_delimiter(checkpoint.clone(), &header, delimiter_utf16.end);
        self.current = Some(ActiveTable {
            header,
            checkpoint,
            header_utf16,
            delimiter_utf16,
            body: Vec::new(),
            inline_rows: Vec::new(),
            found,
            include_header,
            first_overflow: None,
            base_boundary: Some(boundary.clone()),
            boundary: Some(boundary),
            halo_boundary: None,
            chunk_boundary: None,
            chunk_halo: None,
        });
        Ok(())
    }

    fn advance_coordinates(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<MarkdownTableStructureStep, MarkdownTableRequestError> {
        let coordinate = self
            .coordinates
            .as_mut()
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        let (work, done) = coordinate.advance(document, budget)?;
        if matches!(self.target, MarkdownTableWindowTarget::FirstTableInViewport)
            && coordinate.boundary_index > 0
            && coordinate.values[0] >= self.viewport.end
        {
            // The first verified header is beyond the viewport. Actual source
            // order excludes a table in the preceding gap; do not decode the rest
            // of an offscreen giant header/delimiter or create another checkpoint.
            if !self.continuous {
                self.coordinates = None;
                self.global = None;
            }
            // A continuous batch keeps that verified next header and unfinished
            // coordinate cursor, so later viewports do not restart from source zero.
            self.current = None;
            self.no_table = true;
            return Ok(MarkdownTableStructureStep::NotTable { work });
        }
        if done {
            let coordinate = self
                .coordinates
                .take()
                .ok_or(MarkdownTableRequestError::InvalidRequest)?;
            self.install_header(coordinate.header, coordinate.values)?;
        }
        Ok(MarkdownTableStructureStep::Progress { work })
    }

    fn finish(
        &mut self,
        next_required_row: Option<usize>,
        table_end: Option<VerifiedTableEnd>,
    ) -> Result<(), MarkdownTableRequestError> {
        let current = self
            .current
            .as_mut()
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        if !current.found {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        // Continuous consumption transfers only this window's metadata. The active
        // table retains the same frontier and accepted boundary. The caller saved
        // the first unconsumed Row in pending; the next window must consume it,
        // not resume from a checkpoint already beyond it.
        self.ready = Some(MarkdownTableStructuralWindow {
            source: self.source.clone(),
            header: current.header,
            checkpoint: current.checkpoint.clone(),
            header_utf16: current.header_utf16.clone(),
            delimiter_utf16: current.delimiter_utf16.clone(),
            body: std::mem::take(&mut current.body),
            inline_rows: std::mem::take(&mut current.inline_rows),
            include_header: current.include_header,
            next_required_row: next_required_row.or(current.first_overflow),
            table_end,
            replay: Some(ReplayAnchors {
                header: current.header,
                header_utf16: current.header_utf16.clone(),
                delimiter_utf16: current.delimiter_utf16.clone(),
                base: current
                    .base_boundary
                    .clone()
                    .ok_or(MarkdownTableRequestError::InvalidRequest)?,
                chunk: current
                    .chunk_boundary
                    .clone()
                    .or_else(|| current.base_boundary.clone())
                    .ok_or(MarkdownTableRequestError::InvalidRequest)?,
                halo: current.chunk_halo.clone(),
            }),
        });
        if !self.continuous {
            // Standalone semantics still need same-source code classification.
            // Release the original Global when that transaction ends, not by
            // substituting a second producer for this structural result.
            self.current = None;
        } else if self
            .ready
            .as_ref()
            .is_some_and(|ticket| ticket.table_end.is_some())
        {
            // Actual Global Ended restored the outer spine. Keep Global for the
            // next block without retaining a table that is no longer active.
            self.current = None;
        } else {
            let current = self.current.as_mut().unwrap();
            current.include_header = false;
            current.first_overflow = None;
            current.found = false;
            current.chunk_boundary = None;
            current.chunk_halo = None;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn required_row_end_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<Option<&VerifiedTableEnd>, MarkdownTableRequestError> {
        if !self.source.is_current(document) {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        if let Some(error) = self.failed {
            return Err(error);
        }
        Ok(self.required_row_end.as_ref())
    }

    /// Borrow only under the same Source root/generation. The returned receipt is
    /// still structural, not a semantic window authorized to hide source text.
    #[cfg(test)]
    pub(crate) fn structural_window_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<Option<&MarkdownTableStructuralWindow>, MarkdownTableRequestError> {
        if !self.source.is_current(document) {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        if let Some(error) = self.failed {
            return Err(error);
        }
        Ok(self.ready.as_ref())
    }
}

fn validate_columns(columns: usize) -> Result<(), MarkdownTableRequestError> {
    if columns == 0 || columns > TABLE_WINDOW_MAX_COLUMNS {
        Err(MarkdownTableRequestError::Window(
            MarkdownTableWindowError::ColumnLimit,
        ))
    } else {
        Ok(())
    }
}
fn global_work(work: GlobalWork, source_tree_steps: usize) -> MarkdownTableRequestWork {
    // Directory descent and fixed control belong to this same step. These are
    // logical work units, not a measurement of OS page residency or elapsed CPU time.
    MarkdownTableRequestWork {
        byte_reads: work.byte_reads,
        transitions: work.transitions + source_tree_steps + 64,
        source_tree_steps,
    }
}

// Reserve the original AVL path plus bounded shared-directory work. Actual text
// borrowing remains within one logical 4 KiB piece, independently of OS page size.
fn tree_height(document: &MarkdownDocumentBuffer) -> usize {
    document.root.as_ref().map_or(0, |root| {
        // A single ordinary Leaf has no compact directory. Other paths reserve
        // that bounded directory's worst-case work in addition to tree descent.
        let directory = if matches!(root.as_ref(), MarkdownPieceTreeNode::Leaf(_)) {
            0
        } else {
            ORIGINAL_SPAN_METADATA_STEPS
        };
        usize::from(root.height()) + directory
    })
}
fn child_budget(budget: usize, tree: usize) -> Result<usize, MarkdownTableRequestError> {
    // Admit indivisible control/tree work before handing the remaining fuel to
    // the original Scanner, whose real minimum cannot be borrowed for outer work.
    let child = budget
        .checked_sub(tree + 64)
        .ok_or(MarkdownTableRequestError::InvalidRequest)?;
    if child < 2048 {
        return Err(MarkdownTableRequestError::InvalidRequest);
    }
    Ok(child)
}

struct HeaderCoordinates {
    header: GlobalTableFact,
    boundaries: [usize; 4],
    values: [usize; 4],
    boundary_index: usize,
    next: usize,
    utf16: usize,
}
impl HeaderCoordinates {
    fn new(
        document: &MarkdownDocumentBuffer,
        header: GlobalTableFact,
        budget: usize,
    ) -> Result<Self, MarkdownTableRequestError> {
        if tree_height(document).saturating_add(64) > budget {
            // Admit directory traversal itself; few byte reads cannot conceal
            // an arbitrarily deep tree walk in this step's work accounting.
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        let mut node = document
            .root
            .as_deref()
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        let mut local = header.header_byte_start;
        let mut before_utf16 = 0;
        // Cached AVL height bounds this path. Add complete left-subtree metrics,
        // without decoding an entire leaf just to locate the starting prefix.
        while let MarkdownPieceTreeNode::Branch { left, right, .. } = node {
            if local < left.metrics().byte_len {
                // The position lies in the left subtree; right metrics contribute
                // nothing to the prefix before the requested source boundary.
                node = left;
            } else {
                // Reuse the complete left aggregate and carry only the remaining
                // byte offset into the right subtree.
                local -= left.metrics().byte_len;
                before_utf16 += left.metrics().utf16_len;
                node = right;
            }
        }
        if let MarkdownPieceTreeNode::OriginalSpan(span) = node {
            // The compact span compresses metadata only. Measure from the hit
            // 4 KiB piece instead of borrowing a whole 64 KiB span prefix as text.
            let hit = span
                .locate_byte(local, true)
                .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
            local = hit.local;
            before_utf16 += hit.prefix.utf16_len;
        }
        Ok(Self {
            header,
            boundaries: [
                header.header_byte_start,
                header.header_byte_end,
                header.separator_byte_start,
                header.separator_byte_end,
            ],
            values: [0; 4],
            boundary_index: 0,
            next: header.header_byte_start - local,
            utf16: before_utf16,
        })
    }

    fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<(MarkdownTableRequestWork, bool), MarkdownTableRequestError> {
        let source_tree_steps = tree_height(document);
        let child = child_budget(budget, source_tree_steps)?;
        let frame = document
            .table_source_frame(self.next, child / 2)
            .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
        let mut work = MarkdownTableRequestWork {
            byte_reads: 0,
            transitions: source_tree_steps + 64,
            source_tree_steps,
        };
        loop {
            // Adjacent header-end/delimiter-start records may name one boundary
            // twice. Four fixed slots suffice regardless of the source length.
            while self.boundary_index < 4 && self.boundaries[self.boundary_index] == self.next {
                self.values[self.boundary_index] = self.utf16;
                self.boundary_index += 1;
            }
            if self.boundary_index == 4 {
                frame
                    .finish()
                    .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
                return Ok((work, true));
            }
            if work.transitions + 2 > budget {
                frame
                    .finish()
                    .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
                return Ok((work, false));
            }
            let Some(byte) = frame.bytes.get(self.next - frame.byte_start) else {
                frame
                    .finish()
                    .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
                return Ok((work, false));
            };
            // Piece construction/editing already validated UTF-8, and actual
            // header/delimiter endpoints are scalar boundaries. Count leading
            // bytes only: a four-byte scalar contributes two UTF-16 units even
            // when a frame boundary cuts its continuation bytes.
            self.utf16 += if *byte < 0x80 || *byte >= 0xc0 {
                if *byte >= 0xf0 { 2 } else { 1 }
            } else {
                0
            };
            self.next += 1;
            work.byte_reads += 1;
            work.transitions += 2;
        }
    }
}

#[cfg(test)]
mod tests;

mod semantic;
pub use semantic::{
    MarkdownTableViewportBudget, MarkdownTableViewportCoverage, MarkdownTableViewportRequest,
    MarkdownTableViewportRequiredRowQuery, MarkdownTableViewportRequiredRowResult,
    MarkdownTableViewportStep, MarkdownTableWindowRequest, MarkdownTableWindowStep, SemanticWork,
    VerifiedMarkdownTableGap, VerifiedMarkdownTableViewport, VerifiedMarkdownTableWindow,
};
