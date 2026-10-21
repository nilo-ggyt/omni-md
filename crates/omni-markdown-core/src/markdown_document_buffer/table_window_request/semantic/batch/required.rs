//! Query one row under its original publication, sharing semantics/quota between table replay and visible windows.

use super::super::super::checkpoint::{ReplayOutcome, ReplayRequest};
use super::*;

/// Retain original weak publication/member identity; a requested ordinal is still unverified and cannot authorize editing.
pub struct MarkdownTableViewportRequiredRowQuery {
    publication: Weak<Publication>,
    origin: QueryOrigin,
    header: usize,
    origin_row: usize,
    requested_row: usize,
}

/// Keep each production path's original weak ticket, without fabricating a projected window for structure navigation.
enum QueryOrigin {
    Window(Weak<WindowRecord>),
    Structure(Weak<MarkdownTableStructuralWindow>),
}

/// Retain only the admitted actual origin; cancellation drops it and its shared checkpoint pages with RequiredState.
enum RequiredOrigin {
    Window(Rc<WindowRecord>),
    Structure(Rc<MarkdownTableStructuralWindow>),
}

impl RequiredOrigin {
    fn header(&self) -> usize {
        match self {
            Self::Window(record) => record.window.presentation.header_byte_range.start,
            Self::Structure(ticket) => ticket.header.header_byte_start,
        }
    }

    fn replay(&self) -> Option<&ReplayAnchors> {
        match self {
            Self::Window(record) => record.window.replay.as_ref(),
            Self::Structure(ticket) => ticket.replay.as_ref(),
        }
    }
}

#[derive(Clone, Copy)]
pub(in crate::markdown_document_buffer::table_window_request) enum RequiredLocation {
    Window(usize),
    Structure(usize),
}

/// Borrow a window index or actual End from the same Ready, without transferring a separately built window.
pub enum MarkdownTableViewportRequiredRowResult<'a> {
    Row {
        window_index: usize,
        row_ordinal: usize,
    },
    TableEnd {
        end: &'a VerifiedTableEnd,
        terminal_window_index: usize,
    },
}

pub(in crate::markdown_document_buffer::table_window_request) enum RequiredPublication {
    Row {
        location: RequiredLocation,
        row_ordinal: usize,
    },
    End {
        end: VerifiedTableEnd,
        terminal: RequiredLocation,
    },
}

pub(super) struct RequiredState {
    origin: RequiredOrigin,
    requested_row: usize,
    resolved: bool,
    end: Option<VerifiedTableEnd>,
    replay: Option<ReplayRequest>,
}
impl RequiredState {
    pub(super) fn keeps(&self, record: &Rc<WindowRecord>) -> bool {
        // Preserve the draft's actual origin plus the target/terminal row.
        // Pruning cannot leave a published required result pointing outside its retained directory.
        matches!(&self.origin, RequiredOrigin::Window(origin) if Rc::ptr_eq(record, origin))
            || (record.window.presentation.header_byte_range.start == self.origin.header()
                && (record.window.presentation.row(self.requested_row).is_some()
                    || self.end.as_ref().is_some_and(|end| {
                        record
                            .window
                            .presentation
                            .row(end.last_body_row_ordinal().unwrap_or(0))
                            .is_some()
                    })))
    }

    pub(super) fn keeps_structure(&self, ticket: &Rc<MarkdownTableStructuralWindow>) -> bool {
        // Target and true terminal remain part of this navigation result even
        // outside the viewport; do not discard them before publication.
        matches!(&self.origin, RequiredOrigin::Structure(origin) if Rc::ptr_eq(ticket, origin))
            || (ticket.header.header_byte_start == self.origin.header()
                && (contains_structure_row(ticket, self.requested_row)
                    || self.end.as_ref().is_some_and(|end| {
                        contains_structure_row(ticket, end.last_body_row_ordinal().unwrap_or(0))
                    })))
    }
}

fn contains_structure_row(ticket: &MarkdownTableStructuralWindow, ordinal: usize) -> bool {
    // Context-only header metadata is not row zero. Other ordinals must occur
    // among this ticket's actual verified body members.
    (ordinal == 0 && ticket.include_header)
        || ticket.body.iter().any(|row| row.row_ordinal == ordinal)
}

impl VerifiedMarkdownTableViewport {
    /// Issue from an actual origin row, never another producer's Ready or an equal-length replacement range.
    pub fn required_row_query_for(
        &self,
        document: &MarkdownDocumentBuffer,
        window_index: usize,
        origin_row: usize,
        requested_row: usize,
    ) -> Result<MarkdownTableViewportRequiredRowQuery, MarkdownTableRequestError> {
        self.check(document)?;
        let record = self
            .windows
            .get(window_index)
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        if record.window.presentation.row(origin_row).is_none() || record.window.replay.is_none() {
            // A context-only header is not editable row zero. An actual member
            // and retained real checkpoint are both required for origin permission.
            return Err(MarkdownTableRequestError::CoverageRequiresCheckpoint);
        }
        Ok(MarkdownTableViewportRequiredRowQuery {
            publication: Rc::downgrade(&self.publication),
            origin: QueryOrigin::Window(Rc::downgrade(record)),
            header: record.window.presentation.header_byte_range.start,
            origin_row,
            requested_row,
        })
    }

    pub fn required_row_result_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<Option<MarkdownTableViewportRequiredRowResult<'_>>, MarkdownTableRequestError> {
        self.check(document)?;
        self.required
            .as_ref()
            .map(|result| match result {
                RequiredPublication::Row {
                    location: RequiredLocation::Window(window_index),
                    row_ordinal,
                } => Ok(MarkdownTableViewportRequiredRowResult::Row {
                    window_index: *window_index,
                    row_ordinal: *row_ordinal,
                }),
                RequiredPublication::End {
                    end,
                    terminal: RequiredLocation::Window(terminal_window_index),
                } => Ok(MarkdownTableViewportRequiredRowResult::TableEnd {
                    end,
                    terminal_window_index: *terminal_window_index,
                }),
                _ => Err(MarkdownTableRequestError::InvalidRequest), // Structure results cannot fabricate full-window indexes.
            })
            .transpose()
    }

    /// Accept this batch's actual structural member; the target integer still requires original replay to return Row/End.
    pub(in crate::markdown_document_buffer::table_window_request) fn required_structure_query_for(
        &self,
        document: &MarkdownDocumentBuffer,
        origin: &Rc<MarkdownTableStructuralWindow>,
        origin_row: usize,
        requested_row: usize,
    ) -> Result<MarkdownTableViewportRequiredRowQuery, MarkdownTableRequestError> {
        self.check(document)?;
        if !self
            .structures
            .iter()
            .any(|ticket| Rc::ptr_eq(ticket, origin))
            || !contains_structure_row(origin, origin_row)
            || origin.replay.is_none()
        {
            return Err(MarkdownTableRequestError::PublicationSuperseded);
        }
        Ok(MarkdownTableViewportRequiredRowQuery {
            publication: Rc::downgrade(&self.publication),
            origin: QueryOrigin::Structure(Rc::downgrade(origin)),
            header: origin.header.header_byte_start,
            origin_row,
            requested_row,
        })
    }

    /// Let Source wrap the same immutable publication's structural location, without exposing a bare index or End.
    pub(in crate::markdown_document_buffer::table_window_request) fn required_publication_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<Option<&RequiredPublication>, MarkdownTableRequestError> {
        self.check(document)?;
        Ok(self.required.as_ref())
    }

    /// Associate End only with this batch's actual terminal row, never another table or older publication for append.
    pub(crate) fn required_end_for_window(
        &self,
        document: &MarkdownDocumentBuffer,
        window_index: usize,
    ) -> Result<Option<&VerifiedTableEnd>, MarkdownTableRequestError> {
        self.check(document)?;
        let window = self
            .windows
            .get(window_index)
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        if let Some(RequiredPublication::End {
            end,
            terminal: RequiredLocation::Window(index),
        }) = &self.required
            && *index == window_index
            && end.header_byte_start() == window.window.presentation.header_byte_range.start
        {
            return Ok(Some(end));
        }
        window.window.table_end_for(document)
    }
}

impl MarkdownTableViewportRequest {
    /// Extend one row under the latest publication and original context/selection, leaving old Ready readable and main Global forward.
    /// Replay distant targets from the real post-delimiter checkpoint; worst-case work is linear, and missing checkpoints reject explicitly.
    pub fn extend_from_ready_with_required_row(
        &mut self,
        document: &MarkdownDocumentBuffer,
        previous: &VerifiedMarkdownTableViewport,
        viewport: Range<usize>,
        query: MarkdownTableViewportRequiredRowQuery,
    ) -> Result<(), MarkdownTableRequestError> {
        self.check_extension(document, previous, &viewport)?;
        if !query
            .publication
            .ptr_eq(&Rc::downgrade(&previous.publication))
        {
            return Err(MarkdownTableRequestError::PublicationSuperseded);
        }
        let origin = match &query.origin {
            // Match the actual weak origin against retained members of this
            // publication, not a caller-supplied header/ordinal pair alone.
            QueryOrigin::Window(identity) => previous
                .windows
                .iter()
                .find(|record| {
                    identity.ptr_eq(&Rc::downgrade(record))
                        && record.window.presentation.header_byte_range.start == query.header
                        && record.window.presentation.row(query.origin_row).is_some()
                })
                .map(|record| RequiredOrigin::Window(Rc::clone(record))),
            QueryOrigin::Structure(identity) => previous
                .structures
                .iter()
                .find(|ticket| {
                    identity.ptr_eq(&Rc::downgrade(ticket))
                        && ticket.header.header_byte_start == query.header
                        && contains_structure_row(ticket, query.origin_row)
                })
                .map(|ticket| RequiredOrigin::Structure(Rc::clone(ticket))),
        }
        .ok_or(MarkdownTableRequestError::PublicationSuperseded)?;
        let anchors = origin
            .replay()
            .ok_or(MarkdownTableRequestError::CoverageRequiresCheckpoint)?;
        // Prepare continuation before mutating the request. Clone only original
        // checkpoint facts/shared spine, never source text or row-history bodies.
        let replay = ReplayRequest::new(self.source.clone(), anchors.clone(), query.requested_row)?;
        self.extend_from_ready(document, previous, viewport)?;
        // Previous remains fully precharged. Admit this replay state and fixed
        // cursor workspace without reissuing a fresh allowance for each row.
        let metadata = std::mem::size_of::<RequiredState>() + 4096;
        if let Err(error) = self.quota.admit_metadata(metadata) {
            // Once extension starts, failure is sticky and returns scratch.
            // Previous publication and canonical text remain independently valid.
            self.failed = Some(MarkdownTableRequestError::Window(error));
            self.structure = None;
            self.windows = Vec::new();
            self.structures = Vec::new();
            self.gaps = Vec::new();
            self.queries = None;
            return Err(MarkdownTableRequestError::Window(error));
        }
        self.required = Some(RequiredState {
            origin,
            requested_row: query.requested_row,
            resolved: false,
            end: None,
            replay: Some(replay),
        });
        if !matches!(self.phase, BatchPhase::Sweep) {
            // An unchanged visible suffix still needs the actual target resolved;
            // pruning cannot publish Ready with its required result missing.
            self.phase = BatchPhase::RequiredRow;
            self.queries = Some(MarkdownTableReferenceQueries::for_batch(Some(
                self.quota.clone(),
            )));
        }
        Ok(())
    }

    /// Search shared Ready and this sweep's admitted tickets for actual members, excluding context-only headers.
    /// Return inspected finite metadata work for the same reserved control allowance.
    fn contains_prepared_row(&self, header: usize, ordinal: usize) -> (bool, usize) {
        let mut work = 0;
        for record in &self.windows {
            work += 1;
            if record.window.presentation.header_byte_range.start == header {
                for row in &record.window.presentation.rows {
                    work += 1;
                    if row.row_ordinal == ordinal && (ordinal != 0 || row.is_header) {
                        return (true, work);
                    }
                }
            }
        }
        for ticket in &self.structures {
            work += 1 + ticket.body.len();
            if ticket.header.header_byte_start == header && contains_structure_row(ticket, ordinal)
            {
                return (true, work);
            }
        }
        for job in &self.jobs {
            work += 1;
            let Some(ticket) = job.as_ref().and_then(|job| job.ticket.as_ref()) else {
                continue; // Only a pending job still owning an actual structural ticket can contain this target.
            };
            if ticket.header.header_byte_start != header {
                continue;
            }
            if ordinal == 0 {
                work += 1;
                if ticket.include_header {
                    return (true, work); // A retained checkpoint cannot replace that job's actual visible-header publication.
                }
            } else {
                for row in &ticket.body {
                    work += 1;
                    if row.row_ordinal == ordinal {
                        return (true, work);
                    }
                }
            }
        }
        (false, work)
    }

    pub(super) fn advance_required_row(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let (header, target) = {
            let required = self
                .required
                .as_ref()
                .ok_or(MarkdownTableRequestError::InvalidRequest)?;
            (required.origin.header(), required.requested_row)
        };
        let (prepared, lookup_work) = self.contains_prepared_row(header, target);
        let mut control = SWEEP_CONTROL_BASE + lookup_work;
        if control > SWEEP_CONTROL_RESERVE {
            return Err(MarkdownTableRequestError::WorkLimit);
        }
        if prepared {
            // Sweep may already have prepared this target. Mark it scheduled for
            // this batch, without issuing an index before projection or duplicating replay.
            let required = self.required.as_mut().unwrap();
            required.resolved = true;
            required.replay = None;
            self.phase = BatchPhase::Prepare;
            self.index = 0;
            return Ok(SemanticWork {
                structural_transitions: control,
                ..SemanticWork::default()
            });
        }
        // End the RequiredState borrow before whole-batch lookup/admission, so
        // mutation across phases does not retain an unrelated mutable borrow.
        let (mut work, outcome) = {
            let required = self.required.as_mut().unwrap();
            let result = required
                .replay
                .as_mut()
                .ok_or(MarkdownTableRequestError::InvalidRequest)?
                .advance(document, budget - SWEEP_CONTROL_RESERVE)?;
            if result.1.is_some() {
                required.resolved = true;
                required.replay = None;
            }
            result
        };
        if let Some(outcome) = outcome {
            match outcome {
                ReplayOutcome::Window(ticket) => {
                    let ticket = *ticket;
                    control += ticket.body.len() * SWEEP_ROW_ADMISSION_COST;
                    // Navigation shares visible jobs' query inventory and total
                    // allowance; it cannot gain another row/cell/run capacity.
                    let cost = self.admit_ticket(&ticket)?;
                    if self.source_structures {
                        control += self.insert_required_structure(ticket);
                    } else {
                        self.job_costs.push(cost);
                        self.jobs.push(Some(self.semantic_job(ticket)));
                    }
                }
                ReplayOutcome::End { end, terminal } => {
                    let terminal_row = end.last_body_row_ordinal().unwrap_or(0);
                    let (exists, checked) =
                        self.contains_prepared_row(end.header_byte_start(), terminal_row);
                    control += checked;
                    if !exists {
                        // Sweep may also hold the true terminal row in a pending
                        // job. Use the same membership rule to avoid admitting it twice.
                        let terminal = *terminal;
                        control += terminal.body.len() * SWEEP_ROW_ADMISSION_COST;
                        let cost = self.admit_ticket(&terminal)?;
                        if self.source_structures {
                            control += self.insert_required_structure(terminal);
                        } else {
                            self.job_costs.push(cost);
                            self.jobs.push(Some(self.semantic_job(terminal)));
                        }
                    }
                    // This RequiredPublication owns the real End association;
                    // do not replace an existing immutable multirow visible ticket.
                    self.required.as_mut().unwrap().end = Some(end);
                }
            }
            self.phase = BatchPhase::Prepare;
            self.index = 0;
        }
        // Batch row slots also bound records/jobs. Two finite directory/member
        // searches, fixed control and actual row handoffs fit the existing 1024
        // reservation. Charge only used control; replay retains its smaller child
        // quantum and the original cumulative fuel is never reissued.
        if control > SWEEP_CONTROL_RESERVE {
            return Err(MarkdownTableRequestError::WorkLimit);
        }
        work.structural_transitions += control;
        Ok(work)
    }

    pub(super) fn finish_required_publication(
        &mut self,
    ) -> Result<Option<RequiredPublication>, MarkdownTableRequestError> {
        // Only a resolved same-batch member becomes the final required result;
        // the requested integer itself cannot create a Row or an EOF terminal.
        let Some(required) = self.required.take() else {
            return Ok(None);
        };
        if !required.resolved {
            return Err(MarkdownTableRequestError::Window(Error::ContextPending));
        }
        let header = required.origin.header();
        if let Some(end) = required.end {
            let row = end.last_body_row_ordinal().unwrap_or(0);
            let terminal = self.required_location(header, row)?;
            return Ok(Some(RequiredPublication::End { end, terminal }));
        }
        let location = self.required_location(header, required.requested_row)?;
        Ok(Some(RequiredPublication::Row {
            location,
            row_ordinal: required.requested_row,
        }))
    }

    /// Locate an admitted actual member, without inventing a full window or inferring source from an ordinal.
    fn required_location(
        &self,
        header: usize,
        ordinal: usize,
    ) -> Result<RequiredLocation, MarkdownTableRequestError> {
        if self.source_structures {
            self.structures
                .iter()
                .position(|ticket| {
                    ticket.header.header_byte_start == header
                        && contains_structure_row(ticket, ordinal)
                })
                .map(RequiredLocation::Structure)
        } else {
            self.windows
                .iter()
                .position(|record| {
                    record.window.presentation.header_byte_range.start == header
                        && record.window.presentation.row(ordinal).is_some()
                })
                .map(RequiredLocation::Window)
        }
        .ok_or(MarkdownTableRequestError::Window(Error::ContextPending))
    }

    /// Insert replayed earlier rows by actual physical start for ordered coverage and downstream flow.
    /// Use the handle slot precharged by admit_ticket; comparisons/Rc moves consume original control, not another index.
    fn insert_required_structure(&mut self, ticket: MarkdownTableStructuralWindow) -> usize {
        let start = structure_range(&ticket, 0)
            .expect("原 Replay 只移交实际行或 H/D")
            .start;
        let mut inspected = 0;
        let index = self
            .structures
            .iter()
            .position(|existing| {
                inspected += 1;
                structure_range(existing, 0)
                    .expect("原结构目录只保真实成员")
                    .start
                    > start
            })
            .unwrap_or(self.structures.len());
        let moved = self.structures.len() - index;
        self.structures.reserve_exact(1);
        self.structures.insert(index, Rc::new(ticket));
        inspected + moved + 1
    }
}
