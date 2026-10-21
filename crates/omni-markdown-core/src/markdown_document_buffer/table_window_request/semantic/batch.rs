//! Batch tables on one real structural frontier; a gap proves no table, not completed ordinary inline semantics.

use super::*;
use crate::markdown_viewport_presentation::table_window::{
    TABLE_WINDOW_MAX_BYTES, TABLE_WINDOW_MAX_UTF16, collect_table_window_reference_queries,
};
use std::rc::{Rc, Weak};

mod required;
use required::RequiredState;
pub use required::{MarkdownTableViewportRequiredRowQuery, MarkdownTableViewportRequiredRowResult};
pub(in crate::markdown_document_buffer::table_window_request) use required::{
    RequiredLocation, RequiredPublication,
};

const MAX_COLUMN_SLOTS: usize = 1024;
// Reserve worst-case finishing work before delegation; unused reservation is not consumed transitions.
const SWEEP_CONTROL_RESERVE: usize = 1024;
// Pay fixed target/end checks, gap decisions, one ticket/job handoff and phase updates.
// This named control fuel is not a measurement of CPU instructions or allocator byte moves.
const SWEEP_CONTROL_BASE: usize = 128;
// Each actual body member pays byte/UTF-16 folds plus row-metadata handoff admission.
const SWEEP_ROW_ADMISSION_COST: usize = 3;
// A last End-owning window may release its bounded spine pages; ordinary shared Rc drops do not pay that cost.
const PRUNE_LAST_SPINE_COST: usize = 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
/// Joint admission includes previous publication's coexisting payload, not allocator capacity or whole-product peak.
/// Ready separately stores retained logical payload for later precharge; pruning cannot refund this construction's peak allowance.
/// The quota covers the supplied previous plus new records, not every historical Ready arbitrarily retained by callers.
pub struct MarkdownTableViewportBudget {
    pub row_slots: usize,
    pub cells: usize,
    pub column_slots: usize,
    pub source_bytes: usize,
    pub source_utf16: usize,
    pub runs: usize,
    pub metadata_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Every interval comes from actual table members or classification, never a convex hull hiding unproven holes.
pub enum MarkdownTableViewportCoverage {
    Table {
        window_index: usize,
        utf16_range: Range<usize>,
    },
    Gap {
        gap_index: usize,
        utf16_range: Range<usize>,
    },
    /// Prove actual physical rows/header context only; each cell's inline semantics still require the same request's work.
    Structure {
        table_index: usize,
        utf16_range: Range<usize>,
    },
}
impl MarkdownTableViewportCoverage {
    pub fn utf16_range(&self) -> &Range<usize> {
        match self {
            Self::Table { utf16_range, .. }
            | Self::Gap { utf16_range, .. }
            | Self::Structure { utf16_range, .. } => utf16_range,
        }
    }
}

/// Construct only from source-order production; no-table proof does not authorize resetting paragraph/container state here.
pub struct VerifiedMarkdownTableGap {
    source: MarkdownTableSourceIdentity,
    bytes: Range<usize>,
    utf16: Range<usize>,
}

// Records retain immutable metadata. Successors clone Rc handles, not row/cell/run payloads or parent-batch chains.
// Source allowance preserves actual copied physical/header input, not a smaller trimmed projection extent.
struct WindowRecord {
    window: VerifiedMarkdownTableWindow,
    retained: MarkdownTableViewportBudget,
}

// Private construction binds publication to this request; source and context still require their own checks.
struct Publication {
    source: MarkdownTableSourceIdentity,
    context_revision: u64,
    selection: Range<usize>,
}
impl VerifiedMarkdownTableGap {
    pub fn byte_range_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<&Range<usize>, MarkdownTableRequestError> {
        self.utf16_range_for(document)?;
        Ok(&self.bytes)
    }
    pub fn utf16_range_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<&Range<usize>, MarkdownTableRequestError> {
        if !self.source.is_current(document) {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        Ok(&self.utf16)
    }
}

/// Windows have completed actual inline context; independent no-table gaps do not authorize hiding ordinary text.
/// Native receives the batch once; cells retain its handle/index without copying whole tables or splitting authority.
pub struct VerifiedMarkdownTableViewport {
    source: MarkdownTableSourceIdentity,
    requested: Range<usize>,
    windows: Vec<Rc<WindowRecord>>,
    structures: Vec<Rc<MarkdownTableStructuralWindow>>,
    gaps: Vec<Rc<VerifiedMarkdownTableGap>>,
    coverage: Vec<MarkdownTableViewportCoverage>,
    budget: MarkdownTableViewportBudget,
    publication: Rc<Publication>,
    // Retain final payload and this publication's own handles/directories, never
    // already-released Parser/reference/underline scratch from a previous batch.
    retained: MarkdownTableViewportBudget,
    required: Option<RequiredPublication>,
}
impl VerifiedMarkdownTableViewport {
    /// Lend original structural tickets only, without projected Strings, Parser graphs or default inline styles.
    pub(in crate::markdown_document_buffer::table_window_request) fn structural_tables(
        &self,
    ) -> &[Rc<MarkdownTableStructuralWindow>] {
        &self.structures
    }
    fn check(&self, document: &MarkdownDocumentBuffer) -> Result<(), MarkdownTableRequestError> {
        self.source
            .is_current(document)
            .then_some(())
            .ok_or(MarkdownTableRequestError::SourceSuperseded)
    }
    pub fn windows_for<'a>(
        &'a self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<
        impl ExactSizeIterator<Item = &'a VerifiedMarkdownTableWindow> + DoubleEndedIterator + use<'a>,
        MarkdownTableRequestError,
    > {
        self.check(document)?;
        Ok(self.windows.iter().map(|record| &record.window))
    }
    pub fn window_at_for(
        &self,
        document: &MarkdownDocumentBuffer,
        index: usize,
    ) -> Result<Option<&VerifiedMarkdownTableWindow>, MarkdownTableRequestError> {
        self.check(document)?;
        Ok(self.windows.get(index).map(|record| &record.window))
    }
    pub fn gaps_for<'a>(
        &'a self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<
        impl ExactSizeIterator<Item = &'a VerifiedMarkdownTableGap> + DoubleEndedIterator + use<'a>,
        MarkdownTableRequestError,
    > {
        self.check(document)?;
        Ok(self.gaps.iter().map(Rc::as_ref))
    }
    pub fn gap_at_for(
        &self,
        document: &MarkdownDocumentBuffer,
        index: usize,
    ) -> Result<Option<&VerifiedMarkdownTableGap>, MarkdownTableRequestError> {
        self.check(document)?;
        Ok(self.gaps.get(index).map(Rc::as_ref))
    }
    pub fn coverage_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<&[MarkdownTableViewportCoverage], MarkdownTableRequestError> {
        self.check(document)?;
        Ok(&self.coverage)
    }
    pub fn requested_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<&Range<usize>, MarkdownTableRequestError> {
        self.check(document)?;
        Ok(&self.requested)
    }
    pub fn budget(&self) -> MarkdownTableViewportBudget {
        self.budget
    }

    fn proves(&self, required: &Range<usize>) -> bool {
        if required.is_empty() {
            return true;
        }
        let mut tables = self
            .windows
            .iter()
            .flat_map(|record| record.window.coverage.iter().cloned())
            .peekable();
        let mut structures = self
            .structures
            .iter()
            .flat_map(|ticket| (0..2).filter_map(move |index| structure_range(ticket, index)))
            .peekable();
        let mut gaps = self.gaps.iter().map(|gap| gap.utf16.clone()).peekable();
        let mut covered = required.start;
        // Merge three original directories by actual physical extent. Structural
        // tickets also prove coverage, but omitted header context/inter-row holes do not.
        // Move only Range scalars without allocating a directory, reading text,
        // or promoting a clipped request into complete-row source permission.
        while let Some((_, kind)) = [
            tables.peek().map(|range| (range.start, 0)),
            structures.peek().map(|range| (range.start, 1)),
            gaps.peek().map(|range| (range.start, 2)),
        ]
        .into_iter()
        .flatten()
        .min()
        {
            let range = match kind {
                0 => tables.next().unwrap(),
                1 => structures.next().unwrap(),
                _ => gaps.next().unwrap(),
            };
            if range.end <= covered {
                continue;
            }
            if range.start > covered {
                return false;
            }
            covered = range.end;
            if covered >= required.end {
                return true;
            }
        }
        false
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownTableViewportStep {
    Progress { work: SemanticWork },
    Ready { work: SemanticWork },
}
#[derive(Clone, Copy)]
enum BatchPhase {
    Sweep,
    RequiredRow,
    Prepare,
    References,
    Project,
    Insert,
    PruneWindows,
    PruneStructures,
    PruneGaps,
    Coverage,
    Ready,
    Consumed,
}

/// Keep one structural Global and, after sealing all batch queries, one real source-order reference pass.
/// Reference resolution may reach EOF; bounded Parser input is neither zero full-source traversal nor per-token work proof.
/// Each Global independently permits 4 MiB of live spine pages: the combined bound is 8 MiB, not one shared 4-MiB stock.
/// Additional logical metadata admission includes bounded workspace reservations, not allocator or product-peak measurement.
pub struct MarkdownTableViewportRequest {
    source: MarkdownTableSourceIdentity,
    structure: Option<MarkdownTableWindowStructureRequest>,
    viewport: Range<usize>,
    context_revision: u64,
    selection: Range<usize>,
    phase: BatchPhase,
    jobs: Vec<Option<MarkdownTableWindowRequest>>,
    job_costs: Vec<MarkdownTableViewportBudget>,
    index: usize,
    queries: Option<MarkdownTableReferenceQueries>,
    windows: Vec<Rc<WindowRecord>>,
    source_structures: bool,
    structures: Vec<Rc<MarkdownTableStructuralWindow>>,
    gaps: Vec<Rc<VerifiedMarkdownTableGap>>,
    coverage: Vec<MarkdownTableViewportCoverage>,
    coverage_window: usize,
    coverage_range: usize,
    coverage_structure: usize,
    coverage_structure_range: usize,
    coverage_gap: usize,
    covered_until: usize,
    prune_index: usize,
    prune_keep: usize,
    between_start: (usize, usize),
    active_header: Option<usize>,
    // Preserve actual physical/classified boundaries from the same frontier in
    // both byte and UTF-16 coordinates; never infer one from the other's numeric value.
    resume_boundary: (usize, usize),
    budget: MarkdownTableViewportBudget,
    records: MarkdownTableViewportBudget,
    quota: super::prefixes::BatchQuota,
    total: SemanticWork,
    ready: Option<VerifiedMarkdownTableViewport>,
    required: Option<RequiredState>,
    pending_record: Option<Rc<WindowRecord>>,
    last_publication: Option<Weak<Publication>>,
    // Only the source-aware owner advances this floor. Structural classification
    // cannot stand in for actual ordinary-source consumption.
    source_origin_floor: usize,
    failed: Option<MarkdownTableRequestError>,
}

impl MarkdownDocumentBuffer {
    pub fn table_viewport_request(
        &self,
        viewport: Range<usize>,
        context_revision: u64,
        active_selection: Range<usize>,
    ) -> Result<MarkdownTableViewportRequest, MarkdownTableRequestError> {
        if active_selection.start > active_selection.end || active_selection.end > self.utf16_len()
        {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        let mut structure = self.table_window_structure_request(
            MarkdownTableWindowTarget::FirstTableInViewport,
            viewport.clone(),
        )?;
        structure.continuous = true;
        let quota = super::prefixes::BatchQuota::new(MAX_RUNS, MAX_METADATA_BYTES);
        quota
            .admit_metadata(publication_metadata())
            .map_err(MarkdownTableRequestError::Window)?;
        Ok(MarkdownTableViewportRequest {
            source: structure.source.clone(),
            structure: Some(structure),
            viewport: viewport.clone(),
            context_revision,
            selection: active_selection,
            phase: BatchPhase::Sweep,
            jobs: Vec::new(),
            job_costs: Vec::new(),
            index: 0,
            queries: Some(MarkdownTableReferenceQueries::for_batch(Some(
                quota.clone(),
            ))),
            windows: Vec::new(),
            source_structures: false,
            structures: Vec::new(),
            gaps: Vec::new(),
            coverage: Vec::new(),
            coverage_window: 0,
            coverage_range: 0,
            coverage_structure: 0,
            coverage_structure_range: 0,
            coverage_gap: 0,
            covered_until: viewport.start,
            prune_index: 0,
            prune_keep: 0,
            between_start: (0, 0),
            active_header: None,
            resume_boundary: (0, 0),
            budget: MarkdownTableViewportBudget::default(),
            records: MarkdownTableViewportBudget::default(),
            quota,
            total: SemanticWork::default(),
            ready: None,
            required: None,
            pending_record: None,
            last_publication: None,
            source_origin_floor: 0,
            failed: None,
        })
    }
}

impl MarkdownTableViewportRequest {
    pub fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        cancelled: bool,
        budget: usize,
    ) -> Result<MarkdownTableViewportStep, MarkdownTableRequestError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        let result = if cancelled {
            Err(MarkdownTableRequestError::Cancelled)
        } else if !self.source.is_current(document) {
            Err(MarkdownTableRequestError::SourceSuperseded)
        } else if !(4096..=65536).contains(&budget) {
            Err(MarkdownTableRequestError::InvalidRequest)
        } else {
            self.advance_quantum(document, budget)
        };
        if let Err(error) = result {
            // First failure is sticky. Return this request's windows, scratch and
            // both Globals on failure/cancellation instead of publishing partial overlays.
            self.failed = Some(error);
            self.structure = None;
            self.jobs = Vec::new();
            self.job_costs = Vec::new();
            self.queries = None;
            self.windows = Vec::new();
            self.structures = Vec::new();
            self.gaps = Vec::new();
            self.coverage = Vec::new();
            self.ready = None;
            self.required = None;
            self.pending_record = None;
            self.last_publication = None;
        }
        result
    }

    fn advance_quantum(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<MarkdownTableViewportStep, MarkdownTableRequestError> {
        let mut work = SemanticWork::default();
        loop {
            // One quantum may advance several zero-source control phases, each
            // still charged. An asynchronous queue alone is not fairness evidence.
            if matches!(self.phase, BatchPhase::Ready) {
                return Ok(MarkdownTableViewportStep::Ready { work });
            }
            if matches!(self.phase, BatchPhase::Consumed) {
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            if self
                .structure
                .as_ref()
                .is_some_and(|structure| structure.ordinary_pending.is_some())
            {
                // The opt-in owner must consume the original ordinary block first;
                // do not loop over the same pending fact or advance Global past it.
                return Ok(MarkdownTableViewportStep::Progress { work });
            }
            let used = work
                .source_bytes
                .max(work.structural_transitions)
                .max(work.copied_source_bytes)
                .max(work.utf8_validation_bytes);
            let remaining = budget.saturating_sub(used);
            // A gap owns only ranges and weak root identity, so pruning it needs
            // fixed control without source reads, spine destruction or callbacks.
            // Consume this quantum's actual remainder rather than reissuing budget
            // per gap; Coverage/other phases restore their ordinary minimum admission.
            let minimum = if matches!(self.phase, BatchPhase::PruneGaps) {
                64
            } else {
                4096
            };
            if remaining < minimum {
                return Ok(MarkdownTableViewportStep::Progress { work });
            }
            let mut measured = self.advance_one(document, remaining)?;
            measured.structural_transitions = measured.structural_transitions.max(64);
            add_work(&mut work, measured)?;
            add_work(&mut self.total, measured)?;
            let structure = self
                .structure
                .as_ref()
                .ok_or(MarkdownTableRequestError::InvalidRequest)?;
            if self.total.source_bytes > structure.max_byte_work
                || self.total.structural_transitions > structure.max_transitions
            {
                return Err(MarkdownTableRequestError::WorkLimit);
            }
        }
    }

    fn advance_one(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let mut work = SemanticWork {
            structural_transitions: 64,
            ..SemanticWork::default()
        };
        match self.phase {
            BatchPhase::Sweep => return self.advance_sweep(document, budget),
            BatchPhase::RequiredRow => return self.advance_required_row(document, budget),
            BatchPhase::Prepare => {
                if self.index == self.jobs.len() {
                    self.queries.as_mut().unwrap().seal_batch();
                    self.index = 0;
                    if self.queries.as_ref().unwrap().is_ready() {
                        self.phase = BatchPhase::Project;
                    } else {
                        // Seal every finite query before the sole reference
                        // Global scans from actual source zero under first-wins rules.
                        let job = self.jobs[0].as_mut().unwrap();
                        job.queries = self.queries.take();
                        // Reserve one reusable raw/prefix/Unicode-normalizer
                        // workspace bound, not one per query/table and not an exact heap measurement.
                        self.quota
                            .admit_metadata(
                                16 * 1024 * (2 * std::mem::size_of::<usize>() + 2)
                                    + 12 * (16 * 1024 + 1000),
                            )
                            .map_err(MarkdownTableRequestError::Window)?;
                        job.references = Some(new_global(document)?);
                        job.phase = Phase::References;
                        self.phase = BatchPhase::References;
                    }
                } else {
                    let job = self.jobs[self.index].as_mut().unwrap();
                    if matches!(job.phase, Phase::Queries) {
                        // Admit directly into the same inventory before its
                        // allocation; do not build a second map and reject only after merging.
                        let queries = self.queries.as_mut().unwrap();
                        work.parser_input_bytes = job.with_input(document, false, |input| {
                            collect_table_window_reference_queries(input, queries)
                        })?;
                        job.phase = Phase::AwaitReferences;
                        self.index += 1;
                    } else {
                        work = step_work(job.advance(document, false, budget)?);
                    }
                }
            }
            BatchPhase::References => {
                let job = self.jobs[0].as_mut().unwrap();
                work = step_work(job.advance(document, false, budget)?);
                if matches!(job.phase, Phase::Underline) {
                    self.queries = job.queries.take();
                    self.phase = BatchPhase::Project;
                    self.index = 0;
                }
            }
            BatchPhase::Project => {
                if self.index == self.jobs.len() {
                    self.jobs = Vec::new();
                    self.job_costs = Vec::new();
                    self.queries = None;
                    self.begin_prune();
                } else {
                    let job = self.jobs[self.index].as_mut().unwrap();
                    if matches!(job.phase, Phase::AwaitReferences) {
                        job.phase = Phase::Underline;
                    }
                    work = step_work(
                        job.advance_with_references(
                            document,
                            false,
                            budget,
                            self.queries.as_ref(),
                            self.structure
                                .as_mut()
                                .and_then(|structure| structure.global.as_mut()),
                        )?,
                    );
                    if matches!(job.phase, Phase::Ready) {
                        let window = job
                            .take_ready_for(document)?
                            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
                        self.budget.runs = self
                            .budget
                            .runs
                            .checked_add(
                                window.presentation.runs.len()
                                    + window.presentation.header_context.runs.len(),
                            )
                            .ok_or(MarkdownTableRequestError::WorkLimit)?;
                        if self.budget.runs > MAX_RUNS {
                            return Err(MarkdownTableRequestError::Window(Error::SemanticLimit));
                        }
                        // admit_ticket owns original-input admission. Final
                        // retained cost counts immutable arrays, not released scratch.
                        let retained = retained_window_budget(&window, self.job_costs[self.index])?;
                        self.records = add_budget(self.records, retained)?;
                        self.pending_record = Some(Rc::new(WindowRecord { window, retained }));
                        self.jobs[self.index] = None;
                        self.phase = BatchPhase::Insert;
                    }
                }
            }
            BatchPhase::Insert => {
                // Replay may return a row before existing windows. Move only
                // bounded Rc handles, never row/cell/run payload. A separate handoff
                // step avoids uncharged directory movement after Parser exhausts its fuel.
                let record = self
                    .pending_record
                    .take()
                    .ok_or(MarkdownTableRequestError::InvalidRequest)?;
                let start = record
                    .window
                    .coverage
                    .first()
                    .map_or(usize::MAX, |range| range.start);
                let at = self.windows.partition_point(|existing| {
                    existing
                        .window
                        .coverage
                        .first()
                        .is_some_and(|range| range.start <= start)
                });
                work.structural_transitions += self.windows.len() * 2 + 1;
                self.windows.insert(at, record);
                self.index += 1;
                self.phase = BatchPhase::Project;
            }
            BatchPhase::PruneWindows => {
                if self.prune_index < self.windows.len() {
                    let record = &self.windows[self.prune_index];
                    // A window has separate header/delimiter and actual body
                    // coverage; an offscreen gap cannot enter their convex hull.
                    if record
                        .window
                        .coverage
                        .iter()
                        .any(|range| intersects(range, &self.viewport))
                        || self
                            .required
                            .as_ref()
                            .is_some_and(|required| required.keeps(record))
                    {
                        self.records = add_budget(self.records, record.retained)?;
                        self.windows.swap(self.prune_keep, self.prune_index);
                        self.prune_keep += 1;
                    }
                    self.prune_index += 1;
                } else if self.windows.len() > self.prune_keep {
                    // Return one departed record per step while previous remains
                    // readable. Charge bounded spine cleanup only when destroying
                    // the final End-owning record; a shared-handle decrement is ordinary control.
                    let last = self.windows.last().unwrap();
                    if Rc::strong_count(last) == 1 && last.window.table_end.is_some() {
                        work.structural_transitions = PRUNE_LAST_SPINE_COST;
                    }
                    drop(self.windows.pop());
                } else {
                    self.prune_index = 0;
                    self.prune_keep = 0;
                    self.phase = BatchPhase::PruneStructures;
                }
            }
            BatchPhase::PruneStructures => {
                if self.prune_index < self.structures.len() {
                    let ticket = &self.structures[self.prune_index];
                    if (0..2)
                        .filter_map(|index| structure_range(ticket, index))
                        .any(|range| intersects(&range, &self.viewport))
                        || self
                            .required
                            .as_ref()
                            .is_some_and(|required| required.keeps_structure(ticket))
                    {
                        self.records = add_budget(self.records, structural_budget(ticket))?;
                        self.structures.swap(self.prune_keep, self.prune_index);
                        self.prune_keep += 1;
                    }
                    self.prune_index += 1;
                } else if self.structures.len() > self.prune_keep {
                    work.structural_transitions = PRUNE_LAST_SPINE_COST;
                    drop(self.structures.pop());
                } else {
                    self.prune_index = 0;
                    self.prune_keep = 0;
                    self.phase = BatchPhase::PruneGaps;
                }
            }
            BatchPhase::PruneGaps => {
                if self.prune_index < self.gaps.len() {
                    if intersects(&self.gaps[self.prune_index].utf16, &self.viewport) {
                        self.records.metadata_bytes = self
                            .records
                            .metadata_bytes
                            .checked_add(gap_metadata())
                            .ok_or(MarkdownTableRequestError::WorkLimit)?;
                        self.gaps.swap(self.prune_keep, self.prune_index);
                        self.prune_keep += 1;
                    }
                    self.prune_index += 1;
                } else if self.gaps.len() > self.prune_keep {
                    drop(self.gaps.pop());
                } else {
                    self.phase = BatchPhase::Coverage;
                }
            }
            BatchPhase::Coverage => {
                // Required-result lookup scans only current bounded handles;
                // account its directory bound separately in this control fuel.
                work.structural_transitions += (self.windows.len() + self.structures.len()) * 2;
                self.advance_coverage(document)?;
            }
            BatchPhase::Ready | BatchPhase::Consumed => {
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
        }
        self.budget.metadata_bytes = self.quota.metadata_used();
        Ok(work)
    }

    fn advance_sweep(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let classified = self.structure.as_ref().and_then(|structure| {
            (structure.capture_ordinary
                && structure.ordinary_pending.is_none()
                && structure.pending.is_none()
                && structure.coordinates.is_none()
                && self.active_header.is_none())
            .then(|| {
                structure
                    .global
                    .as_ref()
                    .map(GlobalFrontier::table_classified_position)
            })
            .flatten()
        });
        if let Some(boundary) = classified.filter(|boundary| {
            boundary.1 >= self.viewport.end && self.source_origin_floor >= self.viewport.end
        }) {
            // Classification may close a gap after the source owner consumed
            // required ordinary rows. It does not imply the origin cursor reached
            // paragraph end; the opt-in owner still exclusively retains that continuation.
            self.push_gap(document, boundary)?;
            self.resume_boundary = boundary;
            self.phase = if self.required.is_some() {
                BatchPhase::RequiredRow
            } else {
                BatchPhase::Prepare
            };
            self.index = 0;
            return Ok(SemanticWork {
                structural_transitions: SWEEP_CONTROL_BASE,
                ..SemanticWork::default()
            });
        }
        let structure = self.structure.as_mut().unwrap();
        // Completed windows still occupy joint batch allowance; another window
        // cannot reacquire a fresh row/cell stock.
        let available_rows = TABLE_WINDOW_MAX_ROWS.saturating_sub(self.budget.row_slots + 2);
        let available_cells_as_rows = structure.current.as_ref().map_or(usize::MAX, |table| {
            TABLE_WINDOW_MAX_CELLS
                .saturating_sub(self.budget.cells)
                .checked_div(table.header.columns)
                .unwrap_or(0)
                .saturating_sub(1)
        });
        structure.continuous_body_limit = Some(available_rows.min(available_cells_as_rows));
        if structure.capture_ordinary
            && available_rows != 0
            && available_cells_as_rows == 0
            && matches!(structure.pending.as_ref(), Some(GlobalFact::Row(row))
                if row.utf16_start < self.viewport.end && self.viewport.start < row.utf16_end)
        {
            // Source-aware joint stock exhausted cells, not row slots. Preserve
            // that cause before the structural layer maps a zero body limit to
            // RowLimit; existing table-only error semantics remain unchanged.
            return Err(MarkdownTableRequestError::Window(Error::CellLimit));
        }
        let ending = match structure.pending.as_ref() {
            Some(GlobalFact::TableEnd(end)) => {
                Some((end.header_byte_start(), end.byte_end(), end.utf16_end()))
            }
            _ => None,
        };
        // Reserve worst-case finishing work, then charge actual control/member
        // work below. NeedInput or zero-source progress does not consume the
        // whole reservation merely because it was held aside.
        let step = structure.advance(document, false, budget - SWEEP_CONTROL_RESERVE)?;
        let mut control_work = SWEEP_CONTROL_BASE;
        let measured = match step {
            MarkdownTableStructureStep::Progress { work }
            | MarkdownTableStructureStep::StructureReady { work }
            | MarkdownTableStructureStep::NotTable { work }
            | MarkdownTableStructureStep::TableEndReached { work } => work,
        };
        if structure.ordinary_pending.is_some() {
            // Transfer the original fact to the source-aware owner first;
            // a classified position cannot skip that actual source processing.
            return Ok(SemanticWork {
                source_bytes: measured.byte_reads,
                structural_transitions: measured.transitions + control_work,
                source_tree_steps: measured.source_tree_steps,
                ..SemanticWork::default()
            });
        }
        if let Some((header, byte, utf16)) = ending {
            if self.active_header != Some(header) {
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            self.between_start = (byte, utf16);
            self.active_header = None;
        }
        let header = structure
            .current
            .as_ref()
            .map(|table| (table.header.header_byte_start, table.header_utf16.start));
        if let Some((byte, utf16)) = header
            && self.active_header != Some(byte)
        {
            self.push_gap(document, (byte, utf16))?;
            self.active_header = Some(byte);
        }
        let structure = self.structure.as_mut().unwrap();
        // Close once actual verified members fully cover the target, without
        // scanning an unrelated following huge row merely to discover its existence.
        let reached = structure.ready.is_none()
            && structure.current.as_ref().is_some_and(|table| {
                table.found
                    && (table.include_header && table.delimiter_utf16.end >= self.viewport.end
                        || table
                            .body
                            .last()
                            .is_some_and(|row| row.utf16_end >= self.viewport.end))
            });
        if reached {
            structure.finish(None, None)?;
        }
        let ticket = structure.ready.take();
        let mut done = reached;
        if let Some(ticket) = ticket {
            control_work += ticket.body.len() * SWEEP_ROW_ADMISSION_COST;
            // Actual member count bounds finishing control within the reservation;
            // the batch also charges delimiter row slots. Check before folds and
            // handoff, never perform excess work and shrink its reported count afterward.
            if control_work > SWEEP_CONTROL_RESERVE {
                return Err(MarkdownTableRequestError::WorkLimit);
            }
            self.resume_boundary = ticket.body.last().map_or(
                (ticket.header.separator_byte_end, ticket.delimiter_utf16.end),
                |row| (row.byte_end, row.utf16_end),
            );
            // Unconsumed rows remain with the same frontier. Stop the sweep only
            // when this actual window covers the requested end.
            if matches!(structure.pending.as_ref(), Some(GlobalFact::Row(row)) if row.utf16_start >= self.viewport.end)
            {
                done = true;
            }
            let cost = self.admit_ticket(&ticket)?;
            if self.source_structures {
                self.structures.reserve_exact(1);
                self.structures.push(Rc::new(ticket)); // Transfer original row tickets without LineCopy or whole-table Parser input.
            } else {
                self.job_costs.push(cost);
                self.jobs.push(Some(self.semantic_job(ticket)));
            }
        }
        let structure = self.structure.as_ref().unwrap();
        let outside_header = structure
            .coordinates
            .as_ref()
            .filter(|coordinates| coordinates.boundary_index > 0)
            .map(|coordinates| (coordinates.header.header_byte_start, coordinates.values[0]));
        if let Some(boundary) = outside_header.filter(|(_, units)| *units >= self.viewport.end) {
            self.push_gap(document, boundary)?;
            self.resume_boundary = boundary;
            done = true;
        } else if self.active_header.is_none()
            && structure.pending.is_none()
            && structure.coordinates.is_none()
        {
            let boundary = structure
                .global
                .as_ref()
                .unwrap()
                .table_classified_position();
            if boundary.1 >= self.viewport.end
                && (!structure.capture_ordinary
                    || self.source_origin_floor >= self.viewport.end
                    || structure.no_table)
            {
                self.push_gap(document, boundary)?;
                self.resume_boundary = boundary;
                done = true;
            }
        }
        if done {
            self.phase = if self.required.is_some() {
                BatchPhase::RequiredRow
            } else {
                BatchPhase::Prepare
            };
            self.index = 0;
        }
        Ok(SemanticWork {
            source_bytes: measured.byte_reads,
            structural_transitions: measured.transitions + control_work,
            source_tree_steps: measured.source_tree_steps,
            ..SemanticWork::default()
        })
    }

    fn push_gap(
        &mut self,
        document: &MarkdownDocumentBuffer,
        end: (usize, usize),
    ) -> Result<(), MarkdownTableRequestError> {
        let start = self.between_start;
        if end.0 < start.0 || end.1 < start.1 {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        if start.1 < end.1 && start.1 < self.viewport.end && self.viewport.start < end.1 {
            let size = gap_metadata();
            self.quota
                .admit_metadata(size + std::mem::size_of::<Rc<VerifiedMarkdownTableGap>>())
                .map_err(MarkdownTableRequestError::Window)?;
            self.records.metadata_bytes = self
                .records
                .metadata_bytes
                .checked_add(size)
                .ok_or(MarkdownTableRequestError::WorkLimit)?;
            self.gaps.push(Rc::new(VerifiedMarkdownTableGap {
                source: self.source.clone(),
                bytes: start.0..end.0,
                utf16: start.1..end.1,
            }));
        }
        if !self.source.is_current(document) {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        Ok(())
    }

    fn admit_ticket(
        &mut self,
        ticket: &MarkdownTableStructuralWindow,
    ) -> Result<MarkdownTableViewportBudget, MarkdownTableRequestError> {
        let columns = ticket.header.columns;
        let rows = ticket
            .body
            .len()
            .checked_add(2)
            .ok_or(MarkdownTableRequestError::WorkLimit)?;
        let cells = (ticket.body.len() + 1)
            .checked_mul(columns)
            .ok_or(MarkdownTableRequestError::WorkLimit)?;
        let bytes = if self.source_structures {
            0
        } else {
            ticket
                .body
                .iter()
                .try_fold(
                    ticket.header.separator_byte_end - ticket.header.header_byte_start,
                    |n, row| n.checked_add(row.byte_end - row.byte_start),
                )
                .ok_or(MarkdownTableRequestError::WorkLimit)?
        };
        let units = if self.source_structures {
            0
        } else {
            ticket
                .body
                .iter()
                .try_fold(
                    ticket.delimiter_utf16.end - ticket.header_utf16.start,
                    |n, row| n.checked_add(row.utf16_end - row.utf16_start),
                )
                .ok_or(MarkdownTableRequestError::WorkLimit)?
        };
        let cost = MarkdownTableViewportBudget {
            row_slots: rows,
            cells,
            column_slots: columns,
            source_bytes: bytes,
            source_utf16: units,
            ..MarkdownTableViewportBudget::default()
        };
        let next = add_budget(self.budget, cost)?;
        if next.row_slots > TABLE_WINDOW_MAX_ROWS {
            return Err(MarkdownTableRequestError::Window(Error::RowLimit));
        }
        if next.cells > TABLE_WINDOW_MAX_CELLS {
            return Err(MarkdownTableRequestError::Window(Error::CellLimit));
        }
        if columns > TABLE_WINDOW_MAX_COLUMNS || next.column_slots > MAX_COLUMN_SLOTS {
            return Err(MarkdownTableRequestError::Window(Error::ColumnLimit));
        }
        if next.source_bytes > TABLE_WINDOW_MAX_BYTES || next.source_utf16 > TABLE_WINDOW_MAX_UTF16
        {
            return Err(MarkdownTableRequestError::Window(Error::SourceLimit));
        }
        // Admit logical structure and final cell/row/origin payload first;
        // LineCopy creates actual bounded source Strings one line at a time later.
        let metadata = if self.source_structures {
            structural_budget(ticket).metadata_bytes
                + std::mem::size_of::<Rc<MarkdownTableStructuralWindow>>()
        } else {
            rows
            * (std::mem::size_of::<TableRowFact>()
                + std::mem::size_of::<VerifiedTableInlineRow>()
                + std::mem::size_of::<
                    crate::markdown_viewport_presentation::table_window::MarkdownTableWindowRow,
                >()
                + std::mem::size_of::<
                    crate::markdown_viewport_presentation::table_window::MarkdownTableOrigin,
                >())
            + cells
                * std::mem::size_of::<
                    crate::markdown_viewport_presentation::table_window::MarkdownTableWindowCell,
                >()
            + std::mem::size_of::<MarkdownTableStructuralWindow>()
            + std::mem::size_of::<WindowRecord>()
            + 2 * std::mem::size_of::<usize>()
            + std::mem::size_of::<Rc<WindowRecord>>()
            + std::mem::size_of::<MarkdownTableViewportBudget>()
        };
        self.quota
            .admit_metadata(metadata)
            .map_err(MarkdownTableRequestError::Window)?;
        self.budget = next;
        Ok(cost)
    }

    fn semantic_job(&self, ticket: MarkdownTableStructuralWindow) -> MarkdownTableWindowRequest {
        MarkdownTableWindowRequest {
            source: self.source.clone(),
            batch: true,
            quota: self.quota.clone(),
            structure: None,
            ticket: Some(ticket),
            scope_cursor: None,
            scope: ScopeProjection::with_quota(self.quota.clone()),
            copier: None,
            lines: Vec::new(),
            prefixes: None,
            prefix_runs: Vec::new(),
            queries: None,
            references: None,
            reference_fact: None,
            pending_reference: None,
            underline: None,
            ready: None,
            required_end: None,
            context_revision: self.context_revision,
            selection: self.selection.clone(),
            phase: Phase::Scope,
            failed: None,
        }
    }

    fn advance_coverage(
        &mut self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<(), MarkdownTableRequestError> {
        // Merge the bounded original source-order streams incrementally, without
        // sorting, convex-hull coverage or traversing the entire batch in one low-fuel step.
        while self.coverage_window < self.windows.len()
            && self.coverage_range == self.windows[self.coverage_window].window.coverage.len()
        {
            self.coverage_window += 1;
            self.coverage_range = 0;
        }
        while self.coverage_structure < self.structures.len()
            && structure_range(
                &self.structures[self.coverage_structure],
                self.coverage_structure_range,
            )
            .is_none()
        {
            self.coverage_structure += 1;
            self.coverage_structure_range = 0;
        }
        let table = if self.source_structures {
            self.structures
                .get(self.coverage_structure)
                .and_then(|ticket| structure_range(ticket, self.coverage_structure_range))
        } else {
            self.windows
                .get(self.coverage_window)
                .and_then(|record| record.window.coverage.get(self.coverage_range))
                .cloned()
        };
        let gap = self
            .gaps
            .get(self.coverage_gap)
            .map(|gap| gap.utf16.clone());
        let take_table = match (&table, &gap) {
            (Some(a), Some(b)) => a.start <= b.start,
            (Some(_), None) => true,
            _ => false,
        };
        let (range, part) = if take_table {
            let range = table.unwrap();
            if self.source_structures {
                self.coverage_structure_range += 1;
            } else {
                self.coverage_range += 1;
            }
            (
                range.clone(),
                if self.source_structures {
                    MarkdownTableViewportCoverage::Structure {
                        table_index: self.coverage_structure,
                        utf16_range: range,
                    }
                } else {
                    MarkdownTableViewportCoverage::Table {
                        window_index: self.coverage_window,
                        utf16_range: range,
                    }
                },
            )
        } else if let Some(range) = gap {
            self.coverage_gap += 1;
            (
                range.clone(),
                MarkdownTableViewportCoverage::Gap {
                    gap_index: self.coverage_gap - 1,
                    utf16_range: range,
                },
            )
        } else {
            if self.covered_until != self.viewport.end {
                return Err(MarkdownTableRequestError::Window(Error::ContextPending));
            }
            self.budget.metadata_bytes = self.quota.metadata_used();
            // Compute this immutable publication's retained logical payload before
            // issuing its private identity. Keep no parent-publication Rc: old
            // Native handles remain readable, while successors own flat record handles and coverage.
            let retained = add_budget(
                self.records,
                MarkdownTableViewportBudget {
                    metadata_bytes: publication_metadata()
                        + self.coverage.len()
                            * std::mem::size_of::<MarkdownTableViewportCoverage>()
                        + self.windows.len() * std::mem::size_of::<Rc<WindowRecord>>()
                        + self.structures.len()
                            * std::mem::size_of::<Rc<MarkdownTableStructuralWindow>>()
                        + self.gaps.len() * std::mem::size_of::<Rc<VerifiedMarkdownTableGap>>(),
                    ..MarkdownTableViewportBudget::default()
                },
            )?;
            let publication = Rc::new(Publication {
                source: self.source.clone(),
                context_revision: self.context_revision,
                selection: self.selection.clone(),
            });
            self.last_publication = Some(Rc::downgrade(&publication));
            let required = self.finish_required_publication()?;
            self.ready = Some(VerifiedMarkdownTableViewport {
                source: self.source.clone(),
                requested: self.viewport.clone(),
                windows: std::mem::take(&mut self.windows),
                structures: std::mem::take(&mut self.structures),
                gaps: std::mem::take(&mut self.gaps),
                coverage: std::mem::take(&mut self.coverage),
                budget: self.budget,
                publication,
                retained,
                required,
            });
            self.phase = BatchPhase::Ready;
            return Ok(());
        };
        let clipped = range.start.max(self.viewport.start)..range.end.min(self.viewport.end);
        if clipped.is_empty() {
            return Ok(());
        }
        if clipped.start != self.covered_until {
            return Err(MarkdownTableRequestError::Window(Error::ProjectionBoundary));
        }
        self.quota
            .admit_metadata(std::mem::size_of::<MarkdownTableViewportCoverage>())
            .map_err(MarkdownTableRequestError::Window)?;
        self.covered_until = clipped.end;
        self.coverage.push(match part {
            MarkdownTableViewportCoverage::Table { window_index, .. } => {
                MarkdownTableViewportCoverage::Table {
                    window_index,
                    utf16_range: clipped,
                }
            }
            MarkdownTableViewportCoverage::Gap { gap_index, .. } => {
                MarkdownTableViewportCoverage::Gap {
                    gap_index,
                    utf16_range: clipped,
                }
            }
            MarkdownTableViewportCoverage::Structure { table_index, .. } => {
                MarkdownTableViewportCoverage::Structure {
                    table_index,
                    utf16_range: clipped,
                }
            }
        });
        if !self.source.is_current(document) {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        Ok(())
    }

    pub fn ready_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<Option<&VerifiedMarkdownTableViewport>, MarkdownTableRequestError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        if !self.source.is_current(document) {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        Ok(self.ready.as_ref())
    }
    pub fn take_ready_for(
        &mut self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<Option<VerifiedMarkdownTableViewport>, MarkdownTableRequestError> {
        self.ready_for(document)?;
        let ready = self.ready.take();
        if ready.is_some() {
            self.phase = BatchPhase::Consumed;
        }
        Ok(ready)
    }
    pub fn forward_resume_floor(&self) -> usize {
        self.resume_boundary.1
    }
    pub fn total_work(&self) -> SemanticWork {
        self.total
    }

    /// Reproject a new selection only over an already-closed gap, without rewinding Global or reviving historical facts.
    /// The caller checks original complete Input/latest publication; this batch also checks its own publication identity.
    pub(in crate::markdown_document_buffer::table_window_request) fn reproject_source_selection(
        &mut self,
        document: &MarkdownDocumentBuffer,
        previous: &VerifiedMarkdownTableViewport,
        viewport: Range<usize>,
        context_revision: u64,
        selection: Range<usize>,
    ) -> Result<bool, MarkdownTableRequestError> {
        previous.check(document)?;
        if !self.source.is_current(document) {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        if !matches!(self.phase, BatchPhase::Consumed)
            || !self
                .last_publication
                .as_ref()
                .is_some_and(|last| last.ptr_eq(&Rc::downgrade(&previous.publication)))
            || self.context_revision != previous.publication.context_revision
            || self.selection != previous.publication.selection
        {
            return Err(MarkdownTableRequestError::PublicationSuperseded);
        }
        // Reuse no-table structure only. Table marker/inline selection needs its
        // own projection; a classified convex hull is not replay inventory and
        // cannot justify another semantic Global for old table payload.
        if !previous.windows.is_empty() || !previous.structures.is_empty() {
            return Ok(false);
        }
        let Some((index, gap)) =
            previous.gaps.iter().enumerate().find(|(_, gap)| {
                gap.utf16.start <= viewport.start && viewport.end <= gap.utf16.end
            })
        else {
            return Ok(false);
        };
        if self
            .structure
            .as_ref()
            .and_then(|structure| structure.global.as_ref())
            .is_none_or(GlobalFrontier::semantic_boundary_retained)
        {
            return Ok(false); // Selection change cannot discard an outstanding semantic-restore obligation.
        }
        // Jointly admit live previous payload and new directories before mutation.
        // Sharing a gap clones only Rc; its original source/byte/UTF-16 facts
        // remain fixed and a new selection cannot enlarge its proven no-table extent.
        let quota = super::prefixes::BatchQuota::new(MAX_RUNS, MAX_METADATA_BYTES);
        quota
            .admit_retained(previous.retained.runs, previous.retained.metadata_bytes)
            .and_then(|()| {
                quota.admit_metadata(
                    publication_metadata() + std::mem::size_of::<Rc<VerifiedMarkdownTableGap>>(),
                )
            })
            .map_err(MarkdownTableRequestError::Window)?;
        let gap = Rc::clone(gap);
        self.viewport = viewport.clone();
        self.context_revision = context_revision;
        self.selection = selection;
        self.gaps = vec![gap];
        self.coverage = Vec::new();
        self.coverage_window = 0;
        self.coverage_range = 0;
        self.coverage_structure = 0;
        self.coverage_structure_range = 0;
        self.coverage_gap = 0;
        self.covered_until = viewport.start;
        self.records = MarkdownTableViewportBudget {
            metadata_bytes: gap_metadata(),
            ..Default::default()
        };
        self.budget = MarkdownTableViewportBudget {
            metadata_bytes: quota.metadata_used(),
            ..Default::default()
        };
        self.quota = quota;
        self.required = None;
        self.last_publication = None;
        // This original gap needs no new structural/reference scan. Coverage still
        // clips incrementally and only normal completion issues a new publication;
        // original cumulative fuel and Global frontier remain in force.
        self.phase = BatchPhase::Coverage;
        self.account_source_work(SemanticWork {
            structural_transitions: SWEEP_CONTROL_BASE + index + 1,
            ..Default::default()
        })?;
        Ok(true)
    }

    /// Reposition the sole Global for a new source target, precharging coexisting old payload without text copies or new scan fuel.
    pub(in crate::markdown_document_buffer::table_window_request) fn reposition_source(
        &mut self,
        document: &MarkdownDocumentBuffer,
        previous: &VerifiedMarkdownTableViewport,
        viewport: Range<usize>,
    ) -> Result<(), MarkdownTableRequestError> {
        previous.check(document)?;
        if !matches!(self.phase, BatchPhase::Consumed)
            || !self
                .last_publication
                .as_ref()
                .is_some_and(|last| last.ptr_eq(&Rc::downgrade(&previous.publication)))
        {
            return Err(MarkdownTableRequestError::PublicationSuperseded);
        }
        let quota = super::prefixes::BatchQuota::new(MAX_RUNS, MAX_METADATA_BYTES);
        quota
            .admit_retained(previous.retained.runs, previous.retained.metadata_bytes)
            .and_then(|()| quota.admit_metadata(publication_metadata()))
            .map_err(MarkdownTableRequestError::Window)?;
        let structure = self.structure.as_mut().expect("来源发布保留唯一结构续体");
        // Even before another source borrow, preserve the original mapping cause;
        // equal revisions cannot replace this actual document root's health.
        document
            .byte_offset_for_utf16(0)
            .map_err(MarkdownTableRequestError::Source)?;
        let work = structure
            .global
            .as_mut()
            .expect("continuous来源保留原Global")
            .restart_from_root(
                GlobalSourceAuthority {
                    current_revision: document.generation(),
                    cancelled: false,
                },
                4096,
            )
            .map_err(MarkdownTableRequestError::Global)?;
        structure.total.transitions = structure
            .total
            .transitions
            .checked_add(work.transitions)
            .ok_or(MarkdownTableRequestError::WorkLimit)?;
        // These unpublished derived values belong to the old target only. Root
        // replay will produce actual table/ordinary facts for the new target.
        structure.viewport = viewport.clone();
        structure.target = MarkdownTableWindowTarget::FirstTableInViewport;
        structure.continuous_body_limit = None;
        structure.pending = None;
        structure.ordinary_pending = None;
        structure.coordinates = None;
        structure.current = None;
        structure.ready = None;
        structure.no_table = false;
        structure.required_row_end = None;
        self.jobs = Vec::new();
        self.job_costs = Vec::new();
        self.windows = Vec::new();
        self.structures = Vec::new();
        self.gaps = Vec::new();
        self.coverage = Vec::new();
        self.index = 0;
        self.coverage_window = 0;
        self.coverage_range = 0;
        self.coverage_structure = 0;
        self.coverage_structure_range = 0;
        self.coverage_gap = 0;
        self.covered_until = viewport.start;
        self.prune_index = 0;
        self.prune_keep = 0;
        self.between_start = (0, 0);
        self.active_header = None;
        self.resume_boundary = (0, 0);
        self.source_origin_floor = 0;
        self.ready = None;
        self.required = None;
        self.pending_record = None;
        self.last_publication = None;
        self.records = MarkdownTableViewportBudget::default();
        self.budget = MarkdownTableViewportBudget {
            metadata_bytes: quota.metadata_used(),
            ..Default::default()
        };
        self.queries = Some(MarkdownTableReferenceQueries::for_batch(Some(
            quota.clone(),
        )));
        self.quota = quota;
        self.viewport = viewport;
        self.phase = BatchPhase::Sweep;
        self.account_source_work(SemanticWork {
            structural_transitions: work.transitions + SWEEP_CONTROL_RESERVE,
            ..Default::default()
        })
    }

    /// Enable capture only through the module's opt-in wrapper, preserving the table-only factory/advance contract.
    pub(in crate::markdown_document_buffer::table_window_request) fn enable_source_capture(
        &mut self,
    ) {
        self.structure.as_mut().unwrap().capture_ordinary = true;
    }

    /// Native paging needs structural rows first; finish cell semantics on the same Global without copying a whole row upfront.
    pub(in crate::markdown_document_buffer::table_window_request) fn enable_structural_tables(
        &mut self,
    ) {
        self.source_structures = true;
    }

    pub(in crate::markdown_document_buffer::table_window_request) fn set_source_origin_floor(
        &mut self,
        floor: usize,
    ) {
        self.source_origin_floor = floor;
    }

    pub(in crate::markdown_document_buffer::table_window_request) fn take_ordinary_source(
        &mut self,
    ) -> Option<CapturedOrdinary> {
        self.structure.as_mut()?.ordinary_pending.take()
    }

    pub(in crate::markdown_document_buffer::table_window_request) fn source_identity(
        &self,
    ) -> MarkdownTableSourceIdentity {
        self.source.clone()
    }

    #[cfg(test)]
    pub(in crate::markdown_document_buffer::table_window_request) fn source_probe_state(
        &self,
    ) -> (usize, usize, usize) {
        let global = self
            .structure
            .as_ref()
            .and_then(|structure| structure.global.as_ref());
        (
            global.map_or(0, |global| std::ptr::from_ref(global).addr()),
            global.map_or(0, GlobalFrontier::next_byte),
            self.quota.metadata_used(),
        )
    }

    /// Briefly lend the original structural Global; semantic lookahead restores it before Ready, without another source graph.
    pub(in crate::markdown_document_buffer::table_window_request) fn source_global_mut(
        &mut self,
    ) -> Result<&mut GlobalFrontier, MarkdownTableRequestError> {
        self.structure
            .as_mut()
            .and_then(|structure| structure.global.as_mut())
            .ok_or(MarkdownTableRequestError::InvalidRequest)
    }

    /// Serial source-semantic jobs share the original batch quota, without rebuilding, refunding or reissuing stock.
    pub(in crate::markdown_document_buffer::table_window_request) fn source_semantic_quota(
        &self,
    ) -> Result<MarkdownTableWindowQuota, MarkdownTableRequestError> {
        if let Some(error) = self.failed {
            // Failure remains the original first cause for every caller; cloning
            // quota cannot bypass this owner's revoked authority.
            return Err(error);
        }
        self.source_limits()?; // The actual structural continuation must still exist; no caller may recreate its stock.
        Ok(self.quota.clone())
    }

    pub(in crate::markdown_document_buffer::table_window_request) fn source_limits(
        &self,
    ) -> Result<(usize, usize), MarkdownTableRequestError> {
        let structure = self
            .structure
            .as_ref()
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        Ok((structure.max_byte_work, structure.max_transitions))
    }

    pub(in crate::markdown_document_buffer::table_window_request) fn admit_source_metadata(
        &mut self,
        bytes: usize,
    ) -> Result<(), MarkdownTableRequestError> {
        self.quota
            .admit_metadata(bytes)
            .map_err(MarkdownTableRequestError::Window)?;
        self.budget.metadata_bytes = self.quota.metadata_used();
        Ok(())
    }

    pub(in crate::markdown_document_buffer::table_window_request) fn admit_source_runs(
        &mut self,
        runs: usize,
    ) -> Result<(), MarkdownTableRequestError> {
        // Plain and table semantics share actual run stock. Source workspace
        // already reserved fixed plain scratch; do not issue another run allowance.
        self.quota
            .admit_retained(runs, 0)
            .map_err(MarkdownTableRequestError::Window)
    }

    pub(in crate::markdown_document_buffer::table_window_request) fn reserve_source_underline_workspace(
        &mut self,
    ) -> Result<(), MarkdownTableRequestError> {
        // Reuse the one underline reservation. After table scratch is returned,
        // plain semantics can take over the same capacity serially.
        self.quota
            .reserve_underline_workspace()
            .map_err(MarkdownTableRequestError::Window)?;
        self.budget.metadata_bytes = self.quota.metadata_used();
        Ok(())
    }

    /// Charge ordinary rereads and coordinate conversion to the same batch total; child limits cannot reissue owner fuel.
    pub(in crate::markdown_document_buffer::table_window_request) fn account_source_work(
        &mut self,
        work: SemanticWork,
    ) -> Result<(), MarkdownTableRequestError> {
        add_work(&mut self.total, work)?;
        let (bytes, transitions) = self.source_limits()?;
        if self.total.source_bytes > bytes || self.total.structural_transitions > transitions {
            return Err(MarkdownTableRequestError::WorkLimit);
        }
        Ok(())
    }

    pub(in crate::markdown_document_buffer::table_window_request) fn abort_source_request(
        &mut self,
        error: MarkdownTableRequestError,
    ) {
        // Return only this request's derived capacity. Exported immutable
        // publication records remain readable under their original source identity.
        self.failed = Some(error);
        self.structure = None;
        self.jobs = Vec::new();
        self.job_costs = Vec::new();
        self.queries = None;
        self.windows = Vec::new();
        self.structures = Vec::new();
        self.gaps = Vec::new();
        self.coverage = Vec::new();
        self.ready = None;
        self.required = None;
        self.pending_record = None;
        self.last_publication = None;
    }

    /// Precharge actual previous payload when source-aware navigation resumes after the structural frontier.
    /// Table-only resume_forward retains its separate independent-batch contract.
    pub(in crate::markdown_document_buffer::table_window_request) fn resume_source_from_ready(
        &mut self,
        document: &MarkdownDocumentBuffer,
        previous: &VerifiedMarkdownTableViewport,
        viewport: Range<usize>,
    ) -> Result<(), MarkdownTableRequestError> {
        previous.check(document)?;
        if !self.source.is_current(document)
            || !matches!(self.phase, BatchPhase::Consumed)
            || !self
                .last_publication
                .as_ref()
                .is_some_and(|last| last.ptr_eq(&Rc::downgrade(&previous.publication)))
            || self.context_revision != previous.publication.context_revision
            || self.selection != previous.publication.selection
        {
            return Err(MarkdownTableRequestError::PublicationSuperseded);
        }
        let mut records = previous.windows.iter().try_fold(
            MarkdownTableViewportBudget {
                metadata_bytes: previous.gaps.len() * gap_metadata(),
                ..MarkdownTableViewportBudget::default()
            },
            |sum, record| add_budget(sum, record.retained),
        )?;
        for ticket in &previous.structures {
            records = add_budget(records, structural_budget(ticket))?;
        }
        let quota = super::prefixes::BatchQuota::new(MAX_RUNS, MAX_METADATA_BYTES);
        quota
            .admit_retained(previous.retained.runs, previous.retained.metadata_bytes)
            .map_err(MarkdownTableRequestError::Window)?;
        quota
            .admit_metadata(
                publication_metadata()
                    + previous.windows.len() * std::mem::size_of::<Rc<WindowRecord>>()
                    + previous.structures.len()
                        * std::mem::size_of::<Rc<MarkdownTableStructuralWindow>>()
                    + previous.gaps.len() * std::mem::size_of::<Rc<VerifiedMarkdownTableGap>>(),
            )
            .map_err(MarkdownTableRequestError::Window)?;
        // Continue the same actual Global only after source/publication and joint
        // stock preflight; rejected admission must not mutate the original request.
        self.resume_forward(
            document,
            viewport,
            self.context_revision,
            self.selection.clone(),
        )?;
        self.windows = previous.windows.clone();
        self.structures = previous.structures.clone();
        self.gaps = previous.gaps.clone();
        self.budget = MarkdownTableViewportBudget {
            metadata_bytes: quota.metadata_used(),
            ..records
        };
        self.records = records;
        self.queries = Some(MarkdownTableReferenceQueries::for_batch(Some(
            quota.clone(),
        )));
        self.quota = quota;
        Ok(())
    }

    fn begin_prune(&mut self) {
        // Do not refund current quota/budget. Recompute only the upcoming retained
        // directory, which a later extension can precharge as its actual retained payload.
        self.records = MarkdownTableViewportBudget::default();
        self.prune_index = 0;
        self.prune_keep = 0;
        // Empty directories have no handles/spine/row payload to return. Begin
        // at actual work rather than spending separate quanta on empty table lists.
        // Nonempty lists still prune and charge per item; original gap/coverage
        // processing does not infer body styles or new no-table authority from emptiness.
        self.phase = if !self.windows.is_empty() {
            BatchPhase::PruneWindows
        } else if !self.structures.is_empty() {
            BatchPhase::PruneStructures
        } else {
            BatchPhase::PruneGaps
        };
    }

    /// Share verified windows from this request's latest Ready, reading only the right suffix from its live Global continuation.
    /// Authority/coverage/quota preflight rejects without mutation; failure/cancellation after progress remains sticky.
    /// Old publication stays readable but cannot fill unknown holes or authorize old styles after selection/context changes.
    /// The caller owns one publication slot and ends old synchronous Rc borrows before progressing; arbitrary history handles are not tracked.
    /// Selection/context are original request identity: SourceIdentity alone cannot detect a same-generation external selection change.
    /// Native must retain complete original selection/Input/context authority instead of treating a current source as sufficient style permission.
    pub fn extend_from_ready(
        &mut self,
        document: &MarkdownDocumentBuffer,
        previous: &VerifiedMarkdownTableViewport,
        viewport: Range<usize>,
    ) -> Result<(), MarkdownTableRequestError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        self.check_extension(document, previous, &viewport)?;
        // Precharge only coexisting immutable payload, without reviving released
        // reference/underline scratch. New suffix queries still need source-zero
        // first-wins resolution; this owner has no document-wide reference index.
        let mut records = previous.windows.iter().try_fold(
            MarkdownTableViewportBudget {
                metadata_bytes: previous.gaps.len() * gap_metadata(),
                ..MarkdownTableViewportBudget::default()
            },
            |sum, record| add_budget(sum, record.retained),
        )?;
        for ticket in &previous.structures {
            records = add_budget(records, structural_budget(ticket))?;
        }
        let quota = super::prefixes::BatchQuota::new(MAX_RUNS, MAX_METADATA_BYTES);
        quota
            .admit_retained(previous.retained.runs, previous.retained.metadata_bytes)
            .map_err(MarkdownTableRequestError::Window)?;
        quota
            .admit_metadata(
                publication_metadata()
                    + previous.windows.len() * std::mem::size_of::<Rc<WindowRecord>>()
                    + previous.structures.len()
                        * std::mem::size_of::<Rc<MarkdownTableStructuralWindow>>()
                    + previous.gaps.len() * std::mem::size_of::<Rc<VerifiedMarkdownTableGap>>(),
            )
            .map_err(MarkdownTableRequestError::Window)?;
        // Copy finite Rc handles only after synchronous preflight succeeds.
        // Clone neither window payload nor a parent-publication chain.
        let windows = previous.windows.clone();
        let gaps = previous.gaps.clone();
        let suffix = viewport.end > self.resume_boundary.1;
        let structure = self.structure.as_mut().unwrap();
        if suffix {
            structure.viewport = self.resume_boundary.1..viewport.end;
            structure.no_table = false;
            if let Some(table) = structure.current.as_mut() {
                // Current header and pending row state still belong to this
                // actual table. Transfer only new body coverage to subsequent semantic jobs.
                table.found = false;
                table.include_header = false;
            }
            if self.active_header.is_none() {
                // The old gap already proves this same-source byte/UTF-16
                // boundary; the new ticket must not cover that original gap twice.
                self.between_start = self.resume_boundary;
            }
        }
        self.required = None;
        self.pending_record = None;
        self.viewport = viewport.clone();
        self.phase = BatchPhase::Sweep;
        self.index = 0;
        self.windows = windows;
        self.structures = previous.structures.clone();
        self.gaps = gaps;
        self.coverage = Vec::new();
        self.coverage_window = 0;
        self.coverage_range = 0;
        self.coverage_structure = 0;
        self.coverage_structure_range = 0;
        self.coverage_gap = 0;
        self.covered_until = viewport.start;
        self.budget = MarkdownTableViewportBudget {
            metadata_bytes: quota.metadata_used(),
            ..records
        };
        self.records = records;
        self.queries =
            suffix.then(|| MarkdownTableReferenceQueries::for_batch(Some(quota.clone())));
        self.quota = quota;
        if !suffix {
            self.begin_prune();
        }
        Ok(())
    }

    fn check_extension(
        &self,
        document: &MarkdownDocumentBuffer,
        previous: &VerifiedMarkdownTableViewport,
        viewport: &Range<usize>,
    ) -> Result<(), MarkdownTableRequestError> {
        previous.check(document)?;
        if !self.source.is_current(document) || !previous.publication.source.is_current(document) {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        if !matches!(self.phase, BatchPhase::Consumed)
            || !self
                .last_publication
                .as_ref()
                .is_some_and(|last| last.ptr_eq(&Rc::downgrade(&previous.publication)))
            || self.context_revision != previous.publication.context_revision
            || self.selection != previous.publication.selection
        {
            return Err(MarkdownTableRequestError::PublicationSuperseded);
        }
        if viewport.start >= viewport.end
            || viewport.end > document.utf16_len()
            || viewport.len() > TABLE_WINDOW_MAX_UTF16
        {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        if self.structure.is_none() {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        // Reuse complete physical coverage, not a clipped requested endpoint.
        // A suffix must meet the actual resume point and every interval to its
        // left must already have independent original coverage proof.
        let required = viewport.start..viewport.end.min(self.resume_boundary.1);
        if viewport.start > self.resume_boundary.1 || !previous.proves(&required) {
            return Err(MarkdownTableRequestError::CoverageRequiresCheckpoint);
        }
        Ok(())
    }

    pub fn resume_forward(
        &mut self,
        document: &MarkdownDocumentBuffer,
        viewport: Range<usize>,
        context_revision: u64,
        selection: Range<usize>,
    ) -> Result<(), MarkdownTableRequestError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        if !self.source.is_current(document) {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        if !matches!(self.phase, BatchPhase::Consumed)
            || viewport.is_empty()
            || viewport.start < self.resume_boundary.1
            || viewport.end > document.utf16_len()
            || viewport.len() > TABLE_WINDOW_MAX_UTF16
            || selection.start > selection.end
            || selection.end > document.utf16_len()
        {
            return Err(MarkdownTableRequestError::InvalidRequest); // Reverse navigation requires an actual checkpoint, never implicit Global recreation.
        }
        let structure = self
            .structure
            .as_mut()
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        structure.viewport = viewport.clone();
        structure.no_table = false;
        if let Some(table) = structure.current.as_mut() {
            table.found = false;
            table.include_header = false;
        }
        self.required = None;
        self.pending_record = None;
        self.viewport = viewport.clone();
        self.context_revision = context_revision;
        self.selection = selection;
        self.phase = BatchPhase::Sweep;
        self.index = 0;
        self.covered_until = viewport.start;
        self.coverage_window = 0;
        self.coverage_range = 0;
        self.coverage_structure = 0;
        self.coverage_structure_range = 0;
        self.coverage_gap = 0;
        self.prune_index = 0;
        self.prune_keep = 0;
        self.budget = MarkdownTableViewportBudget::default();
        self.records = MarkdownTableViewportBudget::default();
        self.quota = super::prefixes::BatchQuota::new(MAX_RUNS, MAX_METADATA_BYTES);
        self.quota
            .admit_metadata(publication_metadata())
            .map_err(MarkdownTableRequestError::Window)?;
        self.queries = Some(MarkdownTableReferenceQueries::for_batch(Some(
            self.quota.clone(),
        )));
        // Explicit nonoverlapping resume starts an independent batch. Old
        // publication stays read-only and cannot be lent back as overlap authority.
        self.last_publication = None;
        Ok(())
    }
}

fn publication_metadata() -> usize {
    // Two usize fields account for logical Rc control overhead, not allocator
    // alignment, bookkeeping or actual whole-product peak memory.
    std::mem::size_of::<VerifiedMarkdownTableViewport>()
        + std::mem::size_of::<Publication>()
        + 2 * std::mem::size_of::<usize>()
}

fn intersects(left: &Range<usize>, right: &Range<usize>) -> bool {
    left.start < right.end && right.start < left.end
}

fn gap_metadata() -> usize {
    std::mem::size_of::<VerifiedMarkdownTableGap>() + 2 * std::mem::size_of::<usize>()
}

/// Structural tickets have header/delimiter and actual contiguous body extents; omitted header context grants no visible flow.
fn structure_range(ticket: &MarkdownTableStructuralWindow, index: usize) -> Option<Range<usize>> {
    if ticket.include_header && index == 0 {
        return Some(ticket.header_utf16.start..ticket.delimiter_utf16.end);
    }
    if index != usize::from(ticket.include_header) {
        return None;
    }
    Some(ticket.body.first()?.utf16_start..ticket.body.last()?.utf16_end)
}

/// Count actual retained row/scope handles only; no copied text means no whole-Parser source-input charge.
fn structural_budget(ticket: &MarkdownTableStructuralWindow) -> MarkdownTableViewportBudget {
    MarkdownTableViewportBudget {
        row_slots: ticket.body.len() + 2,
        cells: (ticket.body.len() + 1) * ticket.header.columns,
        column_slots: ticket.header.columns,
        metadata_bytes: std::mem::size_of::<MarkdownTableStructuralWindow>()
            + 2 * std::mem::size_of::<usize>()
            + ticket.body.capacity() * std::mem::size_of::<TableRowFact>()
            + ticket.inline_rows.capacity() * std::mem::size_of::<VerifiedTableInlineRow>(),
        ..MarkdownTableViewportBudget::default()
    }
}

fn retained_window_budget(
    window: &VerifiedMarkdownTableWindow,
    mut source: MarkdownTableViewportBudget,
) -> Result<MarkdownTableViewportBudget, MarkdownTableRequestError> {
    use crate::markdown_viewport_presentation::table_window::{
        MarkdownTableOrigin, MarkdownTableWindowCell, MarkdownTableWindowRow,
    };
    let presentation = &window.presentation;
    source.runs = presentation.runs.len() + presentation.header_context.runs.len();
    // Retain the structural ticket's actual physical input allowance. Metadata
    // counts final arrays/objects only; returned scope/prefix/reference/underline
    // scratch cannot be counted again as retained payload for the successor.
    let arrays = [
        (
            presentation.rows.len(),
            std::mem::size_of::<MarkdownTableWindowRow>(),
        ),
        (
            presentation.cells.len(),
            std::mem::size_of::<MarkdownTableWindowCell>(),
        ),
        (
            presentation.header_context.cells.len(),
            std::mem::size_of::<MarkdownTableWindowCell>(),
        ),
        (source.runs, std::mem::size_of::<MarkdownPresentationRun>()),
        (
            presentation.origins.len(),
            std::mem::size_of::<MarkdownTableOrigin>(),
        ),
        (window.coverage.len(), std::mem::size_of::<Range<usize>>()),
    ];
    source.metadata_bytes = arrays
        .into_iter()
        .try_fold(
            std::mem::size_of::<WindowRecord>() + 2 * std::mem::size_of::<usize>(),
            |total, (len, size)| {
                len.checked_mul(size)
                    .and_then(|bytes| total.checked_add(bytes))
            },
        )
        .ok_or(MarkdownTableRequestError::WorkLimit)?;
    Ok(source)
}

fn add_budget(
    mut total: MarkdownTableViewportBudget,
    cost: MarkdownTableViewportBudget,
) -> Result<MarkdownTableViewportBudget, MarkdownTableRequestError> {
    macro_rules! add { ($($field:ident),*) => { $(total.$field = total.$field.checked_add(cost.$field).ok_or(MarkdownTableRequestError::WorkLimit)?;)* }; }
    add!(
        row_slots,
        cells,
        column_slots,
        source_bytes,
        source_utf16,
        runs,
        metadata_bytes
    );
    Ok(total)
}

fn step_work(step: MarkdownTableWindowStep) -> SemanticWork {
    match step {
        MarkdownTableWindowStep::Progress { work }
        | MarkdownTableWindowStep::Ready { work }
        | MarkdownTableWindowStep::NotTable { work }
        | MarkdownTableWindowStep::TableEndReached { work } => work,
    }
}
fn add_work(total: &mut SemanticWork, work: SemanticWork) -> Result<(), MarkdownTableRequestError> {
    macro_rules! add { ($($field:ident),*) => { $(total.$field = total.$field.checked_add(work.$field).ok_or(MarkdownTableRequestError::WorkLimit)?;)* }; }
    add!(
        source_bytes,
        copied_source_bytes,
        metadata_bytes,
        utf8_validation_bytes,
        source_tree_steps,
        structural_transitions,
        allocated_source_capacity,
        normalizer_input_bytes,
        parser_input_bytes
    );
    Ok(())
}

#[cfg(test)]
mod tests;
