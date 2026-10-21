//! Prepare one cell at a time in the original SourceRequest, sharing its source, Global context, and cumulative budget.

use super::super::checkpoint::{ReplayOutcome, ReplayRequest};
use super::*;
use crate::{MarkdownPresentationQuery, MarkdownPresentationStyles, MarkdownSourceLink};
use pulldown_cmark::block_frontier::{TableScopeCursor, TableScopeKind, TableScopeStep};

pub enum MarkdownTableCellInlineStep {
    Progress { work: SemanticWork },
    Ready { work: SemanticWork },
}

/// Publish only after the original semantic job completes; a structural row or raw seed alone grants no queryable styles or links.
pub struct VerifiedMarkdownTableCellInline {
    cell: VerifiedMarkdownTableCellSource,
    publication: Rc<SourcePublication>,
    presentation: Option<Box<VerifiedMarkdownPlainViewport>>,
    inherited: MarkdownPresentationStyles,
}

impl VerifiedMarkdownTableCellInline {
    pub fn require_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<(), MarkdownTableRequestError> {
        self.cell.require(document)?;
        self.publication.check(document, authority)
    }
    pub fn cell_source_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<&VerifiedMarkdownTableCellSource, MarkdownTableRequestError> {
        self.require_for(document, authority)?;
        Ok(&self.cell)
    }
    pub fn query_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
        index: usize,
    ) -> Result<Option<MarkdownPresentationQuery>, MarkdownTableRequestError> {
        self.require_for(document, authority)?;
        let Some(presentation) = &self.presentation else {
            return Ok(None);
        };
        Ok(presentation
            .query_for(document, authority, index)?
            .map(|query| MarkdownPresentationQuery {
                utf16_range: query.utf16_range,
                styles: query.styles.union(self.inherited),
            }))
    }
    pub fn link_at_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
        index: usize,
    ) -> Result<Option<&MarkdownSourceLink>, MarkdownTableRequestError> {
        self.require_for(document, authority)?;
        self.presentation.as_ref().map_or(Ok(None), |presentation| {
            presentation.link_at_for(document, authority, index)
        })
    }
}

pub(super) struct CellInlineJob {
    origin: VerifiedMarkdownTableRowSource,
    column: usize,
    replay: Option<ReplayRequest>,
    raw: Option<super::super::cell_edit::MarkdownTableCellEditRequest>,
    scope: Option<TableScopeCursor>,
    inherited: MarkdownPresentationStyles,
    cell: Option<VerifiedMarkdownTableCellSource>,
    // Reuse only selection endpoints actually located by this SourceRequest; the first SourcesOnly cell still locates them itself.
    selection_bytes: Option<Range<usize>>,
    // The previous cell returned this original scanner; current semantics owns it exclusively until publication returns it to the parent.
    underline: Option<crate::markdown_document_buffer::table_source::MarkdownTableUnderlineRequest>,
    semantic: Option<PlainSemanticJob>,
    ready: Option<VerifiedMarkdownTableCellInline>,
}

impl MarkdownViewportSourceRequest {
    /// Require this owner's last consumed publication. Preparation creates replay from its checkpoint without reading text.
    pub fn prepare_cell_inline(
        &mut self,
        document: &MarkdownDocumentBuffer,
        origin: VerifiedMarkdownTableRowSource,
        authority: MarkdownViewportSourceAuthority,
        column: usize,
    ) -> Result<(), MarkdownTableRequestError> {
        self.check(document, authority)?;
        origin.require_for(document, authority)?;
        if !matches!(self.phase, SourcePhase::Consumed)
            || self.cell_inline.is_some()
            || self.row_source_position.is_some()
            || !self
                .last_publication
                .as_ref()
                .is_some_and(|last| last.ptr_eq(&Rc::downgrade(&origin.publication)))
        {
            return Err(MarkdownTableRequestError::PublicationSuperseded);
        }
        let (_, _, columns) = origin.coordinate_for(document, authority)?;
        if column >= columns {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        let anchors = origin
            .ticket
            .replay
            .as_ref()
            .ok_or(MarkdownTableRequestError::CoverageRequiresCheckpoint)?
            .clone();
        let replay = ReplayRequest::new(self.source.clone(), anchors, origin.ordinal)?;
        // The previous job was taken/dropped before this entry, so its fixed workspace must not accumulate per completed cell.
        // Each Ready may remain owned by another Display; its actual published payload still occupies joint inventory.
        self.batch
            .source_semantic_quota()?
            .reserve_cell_inline_workspace(std::mem::size_of::<CellInlineJob>() + 4096)
            .map_err(MarkdownTableRequestError::Window)?;
        self.batch.admit_source_metadata(
            std::mem::size_of::<VerifiedMarkdownTableCellInline>()
                + 2 * std::mem::size_of::<usize>(),
        )?;
        let inherited = if origin.ordinal == 0 {
            MarkdownPresentationStyles::TABLE.union(MarkdownPresentationStyles::TABLE_HEADER)
        } else {
            MarkdownPresentationStyles::TABLE
        };
        self.cell_inline = Some(Box::new(CellInlineJob {
            origin,
            column,
            replay: Some(replay),
            raw: None,
            scope: None,
            inherited,
            cell: None,
            selection_bytes: self.selection_bytes.clone(),
            underline: self.source_underline.take(),
            semantic: None,
            ready: None,
        }));
        Ok(())
    }

    /// Advance one actual child stage through a short borrow of the original Global; failure retires the job and parent continuation.
    pub fn advance_cell_inline(
        &mut self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
        budget: usize,
    ) -> Result<MarkdownTableCellInlineStep, MarkdownTableRequestError> {
        let result = (|| {
            self.check(document, authority)?;
            if !(4096..=65536).contains(&budget) {
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            let limits = self.remaining_source_limits()?;
            let quota = self.batch.source_semantic_quota()?;
            let job = self
                .cell_inline
                .as_mut()
                .ok_or(MarkdownTableRequestError::InvalidRequest)?;
            job.origin.require_for(document, authority)?;
            let work = job.advance(
                document,
                authority,
                budget.min(4096),
                limits,
                quota,
                self.batch.source_global_mut()?,
            )?;
            self.batch.account_source_work(work)?;
            if self.selection_bytes.is_none() {
                // SourcesOnly has no parent semantic-location stage. After the first nonempty cell pays for real location,
                // return its fixed endpoints to this owner for later columns/rows instead of building another cache.
                self.selection_bytes = job.selection_bytes.clone();
            }
            if job.ready.is_some() {
                Ok(MarkdownTableCellInlineStep::Ready { work })
            } else {
                Ok(MarkdownTableCellInlineStep::Progress { work })
            }
        })();
        if let Err(error) = result {
            let error = document
                .original_backing_health()
                .err()
                .map(MarkdownTableRequestError::Source)
                .unwrap_or(error);
            self.fail(error);
            return Err(error);
        }
        result
    }

    pub fn take_cell_inline_ready_for(
        &mut self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<Option<VerifiedMarkdownTableCellInline>, MarkdownTableRequestError> {
        self.check(document, authority)?;
        let ready = self.cell_inline.as_mut().and_then(|job| job.ready.take());
        if let Some(ready) = &ready {
            ready.require_for(document, authority)?;
            self.source_underline = self.cell_inline.as_mut().unwrap().underline.take();
            self.cell_inline = None;
        }
        Ok(ready)
    }
}

impl CellInlineJob {
    /// Replay retains only the target row; scanner, scope, and semantics use separate steps and reuse this request's proved endpoints.
    fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
        budget: usize,
        limits: (usize, usize),
        quota: crate::markdown_viewport_presentation::table_window::MarkdownTableWindowQuota,
        global: &mut GlobalFrontier,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        if self.ready.is_some() {
            return Ok(SemanticWork::default());
        }
        if let Some(replay) = self.replay.as_mut() {
            let (work, outcome) = replay.advance(document, budget - CONTROL_COST)?;
            if let Some(outcome) = outcome {
                let ticket = match outcome {
                    ReplayOutcome::Window(ticket) => *ticket,
                    ReplayOutcome::End { .. } => {
                        return Err(MarkdownTableRequestError::InvalidRequest);
                    }
                };
                self.scope = Some(ticket.checkpoint.scope_cursor());
                self.raw = Some(
                    super::super::cell_edit::MarkdownTableCellEditRequest::from_ticket(
                        ticket,
                        self.origin.ordinal,
                        self.column,
                    ),
                );
                self.replay = None;
            }
            return Ok(work);
        }
        if let Some(scope) = self.scope.as_mut() {
            let step = scope
                .advance(
                    GlobalSourceAuthority {
                        current_revision: document.generation(),
                        cancelled: false,
                    },
                    budget,
                )
                .map_err(MarkdownTableRequestError::Global)?;
            let work = match step {
                TableScopeStep::Progress { work } => work,
                TableScopeStep::Entry { kind, work, .. } => {
                    self.inherited = self.inherited.union(match kind {
                        TableScopeKind::BlockQuote => MarkdownPresentationStyles::BLOCK_QUOTE,
                        TableScopeKind::OrderedList => MarkdownPresentationStyles::ORDERED_LIST,
                        TableScopeKind::UnorderedList => MarkdownPresentationStyles::UNORDERED_LIST,
                        TableScopeKind::Other => MarkdownPresentationStyles::default(),
                    });
                    work
                }
                TableScopeStep::Complete { work } => {
                    self.scope = None;
                    work
                }
            };
            return Ok(SemanticWork {
                structural_transitions: work,
                ..SemanticWork::default()
            });
        }
        if let Some(raw) = self.raw.as_mut() {
            let step = raw.advance(document, false, budget)?;
            let work = match step {
                MarkdownTableCellEditStep::Progress { work }
                | MarkdownTableCellEditStep::Ready { work } => work,
                MarkdownTableCellEditStep::NotTable { .. } => {
                    return Err(MarkdownTableRequestError::InvalidRequest);
                }
            };
            if matches!(step, MarkdownTableCellEditStep::Ready { .. }) {
                let cell = raw
                    .take_cell_source_for(document)?
                    .expect("原 Ready 格来源只消费一次");
                // The raw cell ticket already seals delimiter alignment. Carry it through all soft lines and hidden markers,
                // even though read-only display and the active draft use different Source objects.
                self.inherited = self.inherited.union(match cell.alignment_for(document)? {
                    crate::MarkdownTableAlignment::Left => {
                        MarkdownPresentationStyles::TABLE_ALIGN_LEFT
                    }
                    crate::MarkdownTableAlignment::Center => {
                        MarkdownPresentationStyles::TABLE_ALIGN_CENTER
                    }
                    crate::MarkdownTableAlignment::Right => {
                        MarkdownPresentationStyles::TABLE_ALIGN_RIGHT
                    }
                });
                self.raw = None;
                if cell.range.is_empty() {
                    self.finish(cell, None);
                } else {
                    // Reuse the original ready scanner only for truly monotonic cell coverage; earlier columns need original prefix context again.
                    let mut underline = self.underline.take();
                    if let Some(original) = underline.as_mut()
                        && !original.continue_cell(document, &cell, quota.clone())?
                    {
                        underline = None;
                    }
                    self.semantic = Some(PlainSemanticJob::for_cell(
                        &cell,
                        authority.selection,
                        self.selection_bytes.clone(),
                        underline,
                        quota,
                    )?);
                    self.cell = Some(cell);
                }
            }
            return Ok(work);
        }
        let semantic = self.semantic.as_mut().expect("非空原格必有唯一语义任务");
        if semantic.preparing() {
            let work = semantic.advance_bounds(document, budget)?;
            // Transfer only four completed actual endpoints; an in-flight default 0..0 is not a proved selection.
            self.selection_bytes = semantic.verified_selection_bytes();
            return Ok(work);
        }
        if semantic.done() {
            // Charge temporary-pair cleanup and actual Box publication before completion, within this step and the original quota.
            let work = SemanticWork {
                structural_transitions: CONTROL_COST + semantic.publication_control(document)?,
                metadata_bytes: semantic.publication_payload_metadata(),
                ..SemanticWork::default()
            };
            if work.structural_transitions > budget || work.metadata_bytes > budget {
                return Err(MarkdownTableRequestError::WorkLimit);
            }
            quota
                .admit_metadata(work.metadata_bytes)
                .map_err(MarkdownTableRequestError::Window)?;
            // Finish cell reads/style construction before returning the original frontier; finish publishes actual styles without discarding raw context.
            self.underline = semantic.return_underline();
            let result = semantic.finish(
                self.origin.publication.source.clone(),
                authority.context_revision,
                Rc::downgrade(&self.origin.publication),
            )?;
            let MarkdownOrdinarySemanticState::PlainReady(presentation) = result else {
                return Err(MarkdownTableRequestError::Window(
                    MarkdownTableWindowError::SemanticLimit,
                ));
            };
            let cell = self.cell.take().expect("原非空格随语义任务持活");
            self.semantic = None;
            self.finish(cell, Some(presentation));
            return Ok(work);
        }
        if semantic.needs_underline_workspace() {
            quota
                .reserve_underline_workspace()
                .map_err(MarkdownTableRequestError::Window)?;
        }
        let (work, runs) =
            semantic.advance_after_sources(document, budget, limits, Some(global))?;
        quota
            .admit_retained(runs, 0)
            .map_err(MarkdownTableRequestError::Window)?;
        Ok(work)
    }

    fn finish(
        &mut self,
        cell: VerifiedMarkdownTableCellSource,
        presentation: Option<Box<VerifiedMarkdownPlainViewport>>,
    ) {
        self.ready = Some(VerifiedMarkdownTableCellInline {
            cell,
            publication: Rc::clone(&self.origin.publication),
            presentation,
            inherited: self.inherited,
        });
    }
}
