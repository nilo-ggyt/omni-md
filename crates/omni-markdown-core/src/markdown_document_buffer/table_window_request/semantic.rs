//! Complete a source-bound window through verified structure, bounded lines, scope/references/underline, then metadata publication.

use super::super::{table_reference_request, table_source::MarkdownTableUnderlineRequest};
use super::*;
use crate::markdown_viewport_presentation::table_window::{
    MarkdownProjectedTableWindow, MarkdownTableGlobalContext, MarkdownTableSourceLine,
    MarkdownTableWindowInput, MarkdownTableWindowQuota, project_table_window_with_quota,
    references::MarkdownTableReferenceQueries, table_window_reference_queries,
};
use crate::markdown_viewport_presentation::{MarkdownPresentationRun, MarkdownPresentationStyles};
use pulldown_cmark::block_frontier::{TableScopeCursor, TableScopeStep};
use pulldown_cmark::global_frontier::VerifiedGlobalReference;

mod prefixes;
mod source_lines;
use prefixes::{PrefixRuns, ScopeProjection};
use source_lines::{LineCopy, OwnedLine};

type Error = MarkdownTableWindowError;
// Standalone and batched windows use the same finite projection limits. The
// standalone entry also owns an actual quota rather than borrowing ambient capacity.
const MAX_RUNS: usize = 8192;
const MAX_METADATA_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
/// Separate incremental work from bounded whole-slice Parser input; input limits prove neither AST size nor total RSS.
pub struct SemanticWork {
    pub source_bytes: usize,
    pub copied_source_bytes: usize,
    /// Logical metadata payload handled this step, not Vec capacity, allocator peak, or all CPU work.
    pub metadata_bytes: usize,
    pub utf8_validation_bytes: usize,
    pub source_tree_steps: usize,
    pub structural_transitions: usize,
    pub allocated_source_capacity: usize,
    pub normalizer_input_bytes: usize,
    pub parser_input_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownTableWindowStep {
    Progress { work: SemanticWork },
    Ready { work: SemanticWork },
    NotTable { work: SemanticWork },
    TableEndReached { work: SemanticWork },
}

/// Construct only after every semantic stage succeeds; equal generations cannot authorize another note's window.
/// Ready retains current-window metadata and weak identity, without source text, Parser trees, scope cursors or Native objects.
pub struct VerifiedMarkdownTableWindow {
    source: MarkdownTableSourceIdentity,
    presentation: MarkdownProjectedTableWindow,
    coverage: Vec<Range<usize>>,
    table_end: Option<VerifiedTableEnd>,
    next_required_row: Option<usize>,
    replay: Option<ReplayAnchors>,
}
impl VerifiedMarkdownTableWindow {
    pub fn presentation_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<&MarkdownProjectedTableWindow, MarkdownTableRequestError> {
        // Recheck the captured weak root/generation before exposing metadata;
        // holding a completed window does not retain its document's source authority.
        if !self.source.is_current(document) {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        Ok(&self.presentation)
    }
    pub fn coverage_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<&[Range<usize>], MarkdownTableRequestError> {
        self.presentation_for(document)?;
        Ok(&self.coverage)
    }
    pub fn next_required_row(&self) -> Option<usize> {
        self.next_required_row
    }
    pub fn table_end_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<Option<&VerifiedTableEnd>, MarkdownTableRequestError> {
        self.presentation_for(document)?;
        Ok(self.table_end.as_ref())
    }
}

#[derive(Clone, Copy)]
enum Phase {
    Structure,
    AwaitReferences,
    Scope,
    Copy,
    Prefix,
    Queries,
    References,
    Underline,
    Project,
    Ready,
    Consumed,
    NotTable,
    End,
}

/// Own current-batch scratch and bounded definitions only; source borrows end per step, without timers/workers/Native state.
pub struct MarkdownTableWindowRequest {
    source: MarkdownTableSourceIdentity,
    batch: bool,
    quota: MarkdownTableWindowQuota,
    structure: Option<MarkdownTableWindowStructureRequest>,
    ticket: Option<MarkdownTableStructuralWindow>,
    scope_cursor: Option<TableScopeCursor>,
    scope: ScopeProjection,
    copier: Option<LineCopy>,
    lines: Vec<OwnedLine>,
    prefixes: Option<PrefixRuns>,
    prefix_runs: Vec<MarkdownPresentationRun>,
    queries: Option<MarkdownTableReferenceQueries>,
    references: Option<GlobalFrontier>,
    reference_fact: Option<VerifiedGlobalReference>,
    pending_reference: Option<table_reference_request::Request>,
    underline: Option<MarkdownTableUnderlineRequest>,
    ready: Option<VerifiedMarkdownTableWindow>,
    required_end: Option<VerifiedTableEnd>,
    context_revision: u64,
    selection: Range<usize>,
    phase: Phase,
    failed: Option<MarkdownTableRequestError>,
}

impl MarkdownDocumentBuffer {
    pub fn table_window_request(
        &self,
        target: MarkdownTableWindowTarget,
        viewport: Range<usize>,
        context_revision: u64,
        active_selection: Range<usize>,
    ) -> Result<MarkdownTableWindowRequest, MarkdownTableRequestError> {
        // The original structure request establishes source/viewport authority.
        // This owner adds its own finite semantic quota and no body copy up front.
        let structure = self.table_window_structure_request(target, viewport)?;
        let quota = MarkdownTableWindowQuota::new(MAX_RUNS, MAX_METADATA_BYTES);
        Ok(MarkdownTableWindowRequest {
            source: structure.source.clone(),
            batch: false,
            quota: quota.clone(),
            structure: Some(structure),
            ticket: None,
            scope_cursor: None,
            scope: ScopeProjection::with_quota(quota),
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
            context_revision,
            selection: active_selection,
            phase: Phase::Structure,
            failed: None,
        })
    }

    /// Plan a cell edit only from a verified current-root window, never an unverified candidate for Native publication.
    pub fn plan_verified_table_cell_edit(
        &self,
        window: &VerifiedMarkdownTableWindow,
        row_ordinal: usize,
        column: usize,
        escaped_source: &str,
    ) -> Result<super::super::table_cell_edit::MarkdownTableCellEditPlan, Error> {
        let presentation = window
            .presentation_for(self)
            .map_err(|_| Error::InvalidSource)?;
        let mut plan = self.plan_projected_table_cell_edit(
            &window.source,
            presentation,
            row_ordinal,
            column,
            escaped_source,
        )?;
        plan.cell.context.end = window.table_end.clone();
        Ok(plan)
    }
}

impl MarkdownTableWindowRequest {
    pub fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        cancelled: bool,
        budget: usize,
    ) -> Result<MarkdownTableWindowStep, MarkdownTableRequestError> {
        self.advance_with_references(document, cancelled, budget, None, None)
    }

    fn advance_with_references(
        &mut self,
        document: &MarkdownDocumentBuffer,
        cancelled: bool,
        budget: usize,
        references: Option<&MarkdownTableReferenceQueries>,
        global: Option<&mut GlobalFrontier>,
    ) -> Result<MarkdownTableWindowStep, MarkdownTableRequestError> {
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
            self.advance_inner(document, budget, references, global)
        };
        if let Err(error) = result {
            // Any failed stage permanently retires this request's batch and
            // capacities; later definitions or old Ready values cannot repair it.
            self.failed = Some(error);
            self.clear_scratch();
            self.ready = None;
            self.required_end = None;
        }
        result
    }

    fn advance_inner(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
        references: Option<&MarkdownTableReferenceQueries>,
        global: Option<&mut GlobalFrontier>,
    ) -> Result<MarkdownTableWindowStep, MarkdownTableRequestError> {
        let mut work = SemanticWork::default();
        match self.phase {
            Phase::Structure => {
                let structure = self
                    .structure
                    .as_mut()
                    .ok_or(MarkdownTableRequestError::InvalidRequest)?;
                let step = structure.advance(document, false, budget)?;
                let measured = match step {
                    MarkdownTableStructureStep::Progress { work }
                    | MarkdownTableStructureStep::StructureReady { work }
                    | MarkdownTableStructureStep::NotTable { work }
                    | MarkdownTableStructureStep::TableEndReached { work } => work,
                };
                work.source_bytes = measured.byte_reads;
                work.structural_transitions = measured.transitions;
                work.source_tree_steps = measured.source_tree_steps;
                match step {
                    MarkdownTableStructureStep::StructureReady { .. } => {
                        self.ticket = structure.ready.take();
                        // Semantic work still borrows this same structural Global.
                        // The transferred ticket is released with scratch before Ready.
                        self.phase = Phase::Scope;
                    }
                    MarkdownTableStructureStep::NotTable { .. } => {
                        self.structure = None;
                        self.phase = Phase::NotTable;
                        return Ok(MarkdownTableWindowStep::NotTable { work });
                    }
                    MarkdownTableStructureStep::TableEndReached { .. } => {
                        self.required_end = structure.required_row_end.take();
                        self.structure = None;
                        self.phase = Phase::End;
                        return Ok(MarkdownTableWindowStep::TableEndReached { work });
                    }
                    MarkdownTableStructureStep::Progress { .. } => {}
                }
            }
            Phase::Scope => {
                let ticket = self
                    .ticket
                    .as_ref()
                    .ok_or(MarkdownTableRequestError::InvalidRequest)?;
                if !ticket.is_current(document) {
                    // Request and structural ticket each retain original-source
                    // authority; transferring it cannot make a stale ticket valid for scope work.
                    return Err(MarkdownTableRequestError::SourceSuperseded);
                }
                if self.scope_cursor.is_none() {
                    self.scope_cursor = Some(ticket.checkpoint.scope_cursor());
                    work.structural_transitions = 1024; // Charge this bounded cursor initialization separately from traversal.
                    return Ok(MarkdownTableWindowStep::Progress { work });
                }
                match self
                    .scope_cursor
                    .as_mut()
                    .unwrap()
                    .advance(
                        GlobalSourceAuthority {
                            current_revision: document.generation(),
                            cancelled: false,
                        },
                        budget - 64,
                    )
                    .map_err(MarkdownTableRequestError::Global)?
                {
                    TableScopeStep::Progress { work: measured } => {
                        work.structural_transitions = measured + 64
                    }
                    TableScopeStep::Entry {
                        kind,
                        source_byte_start,
                        work: measured,
                        ..
                    } => {
                        work.metadata_bytes = self
                            .scope
                            .observe(ticket, kind, source_byte_start)
                            .map_err(MarkdownTableRequestError::Window)?;
                        work.structural_transitions = measured + 64;
                    }
                    TableScopeStep::Complete { work: measured } => {
                        self.scope_cursor = None;
                        self.copier =
                            Some(LineCopy::new(ticket).map_err(MarkdownTableRequestError::Window)?);
                        self.phase = Phase::Copy;
                        work.structural_transitions = measured + 64;
                    }
                }
            }
            Phase::Copy => {
                // Copy only actual header/delimiter/selected member lines already
                // admitted by the ticket; omitted table intervals are not input.
                let (measured, complete) = self
                    .copier
                    .as_mut()
                    .ok_or(MarkdownTableRequestError::InvalidRequest)?
                    .advance(document, budget)
                    .map_err(MarkdownTableRequestError::Window)?;
                work = measured;
                if complete {
                    self.lines = self.copier.take().unwrap().into_lines();
                    self.prefixes = Some(std::mem::take(&mut self.scope).into_builder());
                    self.phase = Phase::Prefix;
                }
            }
            Phase::Prefix => {
                // Restore container properties from the original scope and true
                // physical marker coordinates, not a compressed projection's apparent nesting.
                let (measured, complete) = self
                    .prefixes
                    .as_mut()
                    .ok_or(MarkdownTableRequestError::InvalidRequest)?
                    .advance(
                        &self.lines,
                        self.ticket
                            .as_ref()
                            .ok_or(MarkdownTableRequestError::InvalidRequest)?,
                        &self.selection,
                        document.utf16_len(),
                    )
                    .map_err(MarkdownTableRequestError::Window)?;
                work = measured;
                if complete {
                    self.prefix_runs = self.prefixes.take().unwrap().into_runs();
                    self.phase = Phase::Queries;
                }
            }
            Phase::Queries => {
                // Charge bounded projection-parser input separately. These nodes still require
                // actual whole-product peak measurements; local accounting proves no product budget.
                self.queries =
                    Some(self.with_input(document, false, table_window_reference_queries)?);
                work.parser_input_bytes = self
                    .lines
                    .iter()
                    .map(|line| line.text.len() - line.spec.prefix)
                    .sum();
                if self.batch {
                    // Seal every window's labels into the batch's sole inventory
                    // before source-order resolution, preserving earlier first-wins definitions.
                    self.phase = Phase::AwaitReferences;
                } else if self.queries.as_ref().unwrap().is_ready() {
                    self.phase = Phase::Underline;
                } else {
                    self.references = Some(new_global(document)?);
                    self.phase = Phase::References;
                }
            }
            Phase::References => return self.advance_references(document, budget),
            Phase::Underline => {
                if self.underline.is_none() {
                    self.quota
                        .reserve_underline_workspace()
                        .map_err(MarkdownTableRequestError::Window)?;
                    let ticket = self
                        .ticket
                        .as_ref()
                        .ok_or(MarkdownTableRequestError::InvalidRequest)?;
                    self.underline = Some(
                        document
                            .table_underline_request(&byte_coverage(ticket), self.quota.clone())
                            .map_err(MarkdownTableRequestError::Window)?,
                    );
                    work.structural_transitions = 64;
                    return Ok(MarkdownTableWindowStep::Progress { work });
                }
                let global = match global {
                    Some(global) => global, // The batch lends its original Global exclusively to this serial job.
                    None => self
                        .structure
                        .as_mut()
                        .and_then(|structure| structure.global.as_mut())
                        .ok_or(MarkdownTableRequestError::InvalidRequest)?,
                };
                let result =
                    self.underline
                        .as_mut()
                        .unwrap()
                        .advance(document, global, budget - 64)?;
                work.source_bytes = result.source_bytes + result.global_source_bytes;
                work.utf8_validation_bytes = result.utf8_validation_bytes;
                work.source_tree_steps = result.source_tree_steps + result.global_source_tree_steps;
                work.structural_transitions = result.structural_transitions + 64;
                work.copied_source_bytes = result.copied_source_bytes;
                work.metadata_bytes = result.metadata_bytes;
                work.normalizer_input_bytes = result.normalizer_input_bytes;
                work.parser_input_bytes = result.parser_input_bytes;
                work.allocated_source_capacity = result.allocated_source_capacity;
                if result.ready {
                    self.phase = Phase::Project;
                }
            }
            Phase::Project => {
                // Close the real shared inline-stack FD before building the final
                // metadata projection; a computed style is not proof of resource release.
                if let Some(underline) = self.underline.as_mut() {
                    underline.close_inline_stack()?;
                }
                let presentation =
                    self.with_input_references(document, true, references, |input| {
                        project_table_window_with_quota(input, Some(&self.quota))
                    })?;
                work.parser_input_bytes = presentation.projection_bytes;
                let ticket = self
                    .ticket
                    .take()
                    .ok_or(MarkdownTableRequestError::InvalidRequest)?;
                self.ready = Some(VerifiedMarkdownTableWindow {
                    source: self.source.clone(),
                    presentation,
                    coverage: utf16_coverage(&ticket),
                    table_end: ticket.table_end,
                    next_required_row: ticket.next_required_row,
                    replay: ticket.replay,
                });
                // Return source strings, Parser/reference inventory and cursor
                // capacities before Ready, retaining only derived metadata.
                self.clear_scratch();
                self.phase = Phase::Ready;
                return Ok(MarkdownTableWindowStep::Ready { work });
            }
            Phase::Ready => return Ok(MarkdownTableWindowStep::Ready { work }),
            // Transfer Ready once. An empty consumed slot cannot represent
            // another successful batch or authorize another Native publication.
            Phase::Consumed => return Err(MarkdownTableRequestError::InvalidRequest),
            Phase::AwaitReferences => {
                // Only the batch enters this phase; without its complete joint
                // reference inventory, the window remains semantically pending.
                return Err(MarkdownTableRequestError::Window(Error::ContextPending));
            }
            Phase::NotTable => return Ok(MarkdownTableWindowStep::NotTable { work }),
            Phase::End => return Ok(MarkdownTableWindowStep::TableEndReached { work }),
        }
        Ok(MarkdownTableWindowStep::Progress { work })
    }

    fn advance_references(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<MarkdownTableWindowStep, MarkdownTableRequestError> {
        let queries = self
            .queries
            .as_mut()
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        let mut work = SemanticWork::default();
        if let Some(fact) = self.reference_fact.take() {
            // Global already spent its quantum. Admit one real definition fact
            // on the next step, before label/normalizer allocation. Construction
            // failure is sticky; skipping the first definition would change first-wins semantics.
            self.pending_reference =
                table_reference_request::Request::new(self.source.clone(), fact, queries)
                    .map_err(MarkdownTableRequestError::Window)?;
            work.structural_transitions = 64;
        } else if let Some(request) = self.pending_reference.as_mut() {
            let step = request
                .advance(document, queries, budget)
                .map_err(MarkdownTableRequestError::Window)?;
            work.source_bytes = step.work.source_bytes;
            work.copied_source_bytes = step.work.copied_bytes;
            work.metadata_bytes = step.work.metadata_bytes;
            work.source_tree_steps = step.work.source_tree_steps;
            work.structural_transitions = step.work.structural_transitions;
            work.normalizer_input_bytes = step.work.normalizer_input_bytes;
            work.parser_input_bytes = step.work.parser_input_bytes;
            if step.complete {
                self.pending_reference = None;
            }
        } else if queries.is_ready() {
            // Every current label has its actual first-wins result. Stop here
            // rather than building an unnecessary document-wide definition index.
            self.references = None;
            self.phase = Phase::Underline;
        } else {
            let tree = tree_height(document);
            let child = child_budget(budget, tree)?;
            let global = self
                .references
                .as_mut()
                .ok_or(MarkdownTableRequestError::InvalidRequest)?;
            let frame = document
                .table_source_frame(global.next_byte(), child)
                .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
            // This existing boundary maps source acquisition/reclaim failure to
            // InvalidRequest; it does not return the underlying errno as Source.
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
                    child,
                )
                .map_err(MarkdownTableRequestError::Global);
            frame
                .finish()
                .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
            match step? {
                GlobalStep::Progress { work: measured, .. }
                | GlobalStep::NeedInput { work: measured, .. } => {
                    work.source_bytes = measured.byte_reads;
                    work.structural_transitions = measured.transitions + tree + 64;
                }
                GlobalStep::Fact {
                    fact,
                    work: measured,
                } => {
                    work.source_bytes = measured.byte_reads;
                    work.structural_transitions = measured.transitions + tree + 64;
                    if let GlobalFact::Reference(fact) = fact {
                        // Facts come from this original Global under the captured
                        // weak identity, never an externally paired equal-generation fact.
                        self.reference_fact = Some(fact);
                    }
                }
                GlobalStep::Complete { work: measured } => {
                    // Only verified real source EOF closes unresolved definitions
                    // as missing; a short frame or exhausted quantum cannot do so.
                    work.source_bytes = measured.byte_reads;
                    work.structural_transitions = measured.transitions + tree + 64;
                    queries.finish_verified_eof();
                    if !queries.is_ready() {
                        return Err(MarkdownTableRequestError::Window(Error::ReferenceLimit));
                    }
                    self.references = None;
                    self.phase = Phase::Underline;
                }
            }
            work.source_tree_steps = tree;
        }
        Ok(MarkdownTableWindowStep::Progress { work })
    }

    fn with_input<T>(
        &self,
        document: &MarkdownDocumentBuffer,
        ready: bool,
        operation: impl FnOnce(&MarkdownTableWindowInput<'_>) -> Result<T, Error>,
    ) -> Result<T, MarkdownTableRequestError> {
        // Metadata and source strings must still belong to this captured root;
        // recapturing current document state cannot bless a stale semantic ticket.
        self.with_input_references(document, ready, None, operation)
    }

    fn with_input_references<T>(
        &self,
        document: &MarkdownDocumentBuffer,
        ready: bool,
        references: Option<&MarkdownTableReferenceQueries>,
        operation: impl FnOnce(&MarkdownTableWindowInput<'_>) -> Result<T, Error>,
    ) -> Result<T, MarkdownTableRequestError> {
        if !self.source.is_current(document) {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        let ticket = self
            .ticket
            .as_ref()
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        let h = self
            .lines
            .first()
            .ok_or(MarkdownTableRequestError::InvalidRequest)?
            .borrowed();
        let d = self
            .lines
            .get(1)
            .ok_or(MarkdownTableRequestError::InvalidRequest)?
            .borrowed();
        let body = self.lines[2..]
            .iter()
            .map(OwnedLine::borrowed)
            .collect::<Vec<_>>();
        let global = if ready {
            // Final projection needs actual completed underline and reference
            // results. Earlier query collection sees Pending, not invented absence.
            let underline = self
                .underline
                .as_ref()
                .ok_or(MarkdownTableRequestError::InvalidRequest)?
                .ready_pairs_for(document)
                .map_err(MarkdownTableRequestError::Window)?
                .ok_or(MarkdownTableRequestError::Window(Error::ContextPending))?;
            MarkdownTableGlobalContext::ReadyResolved {
                references: references
                    .or(self.queries.as_ref())
                    .ok_or(MarkdownTableRequestError::InvalidRequest)?,
                underline,
            }
        } else {
            MarkdownTableGlobalContext::Pending
        };
        operation(&MarkdownTableWindowInput {
            generation: document.generation(),
            context_revision: self.context_revision,
            header: h,
            delimiter: d,
            body: &body,
            include_header: ticket.include_header,
            inherited_styles: MarkdownPresentationStyles::default(),
            container_prefix_runs: &self.prefix_runs,
            global,
            active_selection: self.selection.clone(),
        })
        .map_err(MarkdownTableRequestError::Window)
    }

    fn clear_scratch(&mut self) {
        // Return only resources owned by this request. Verified output keeps its
        // own immutable metadata and weak source identity, with no scratch backedge.
        self.structure = None;
        self.ticket = None;
        self.scope_cursor = None;
        self.scope = ScopeProjection::default();
        self.copier = None;
        self.lines = Vec::new();
        self.prefixes = None;
        self.prefix_runs = Vec::new();
        self.queries = None;
        self.references = None;
        self.reference_fact = None;
        self.pending_reference = None;
        self.underline = None;
    }
    pub fn ready_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<Option<&VerifiedMarkdownTableWindow>, MarkdownTableRequestError> {
        if !self.source.is_current(document) {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        if let Some(error) = self.failed {
            return Err(error);
        }
        Ok(self.ready.as_ref())
    }
    pub fn take_ready_for(
        &mut self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<Option<VerifiedMarkdownTableWindow>, MarkdownTableRequestError> {
        self.ready_for(document)?;
        let ready = self.ready.take();
        if ready.is_some() {
            // The transferred window retains its original root identity;
            // the request loses permission to publish the same slot again.
            self.phase = Phase::Consumed;
        }
        Ok(ready)
    }
    pub fn required_row_end_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<Option<&VerifiedTableEnd>, MarkdownTableRequestError> {
        if !self.source.is_current(document) {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        if let Some(error) = self.failed {
            return Err(error);
        }
        Ok(self.required_end.as_ref())
    }
}

fn new_global(
    document: &MarkdownDocumentBuffer,
) -> Result<GlobalFrontier, MarkdownTableRequestError> {
    // Reserve source-size-derived cumulative limits for a real source-order
    // traversal. This can reach EOF; it is not whole-text materialization or a
    // claim that local fuel is an allocator/whole-product memory measurement.
    let bytes = document
        .byte_len()
        .checked_mul(256)
        .and_then(|n| n.checked_add(1024 * 1024))
        .ok_or(MarkdownTableRequestError::WorkLimit)?;
    let transitions = document
        .byte_len()
        .checked_mul(2048)
        .and_then(|n| n.checked_add(1024 * 1024))
        .ok_or(MarkdownTableRequestError::WorkLimit)?;
    GlobalFrontier::new(
        document.generation(),
        document.byte_len(),
        crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
        GlobalFrontierLimits {
            spine: StructuralSpineLimits {
                max_depth: 16384,
                max_live_page_bytes: 4 * 1024 * 1024,
                max_page_allocations: 1_000_000,
                max_role_work: transitions,
            },
            max_byte_work: bytes,
            max_transitions: transitions,
        },
    )
    .map_err(MarkdownTableRequestError::Global)
}
fn byte_coverage(ticket: &MarkdownTableStructuralWindow) -> Vec<Range<usize>> {
    // Preserve separate original header/delimiter and actual body intervals;
    // never fill an omitted gap with the convex hull of their coordinates.
    let mut result = Vec::with_capacity(2);
    result.push(ticket.header.header_byte_start..ticket.header.separator_byte_end);
    if let (Some(first), Some(last)) = (ticket.body.first(), ticket.body.last()) {
        result.push(first.byte_start..last.byte_end);
    }
    result
}
fn utf16_coverage(ticket: &MarkdownTableStructuralWindow) -> Vec<Range<usize>> {
    // Header context may be needed for syntax without being visible coverage.
    // Publish it only when the original structural ticket included that header.
    let mut result = Vec::with_capacity(2);
    if ticket.include_header {
        result.push(ticket.header_utf16.start..ticket.delimiter_utf16.end);
    }
    if let (Some(first), Some(last)) = (ticket.body.first(), ticket.body.last()) {
        result.push(first.utf16_start..last.utf16_end);
    }
    result
}

mod batch;
pub use batch::{
    MarkdownTableViewportBudget, MarkdownTableViewportCoverage, MarkdownTableViewportRequest,
    MarkdownTableViewportRequiredRowQuery, MarkdownTableViewportRequiredRowResult,
    MarkdownTableViewportStep, VerifiedMarkdownTableGap, VerifiedMarkdownTableViewport,
};
pub(in crate::markdown_document_buffer::table_window_request) use batch::{
    RequiredLocation, RequiredPublication,
};

#[cfg(test)]
mod tests;
