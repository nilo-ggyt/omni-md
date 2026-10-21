//! Original code proofs and visible intersections for long root paragraphs. Body reads borrow small
//! mmap frames; retained presentation output is bounded by the current viewport.

use super::*;
use pulldown_cmark::global_frontier::{
    InlineCodeCursor, InlineCodeStep, InlineLinkStorageRequest, InlineLinkStorageStep,
    InlineReferenceResolution, VerifiedInlineEmphasisRun, VerifiedInlineLink,
};
pub(in crate::markdown_document_buffer) mod emphasis;
mod link;
mod prefix;
use crate::markdown_document_buffer::table_source::inline_reference::ReferenceLabel;
use crate::markdown_viewport_presentation::table_window::references::MarkdownTableReferenceQueries;
use emphasis::EmphasisJob;
use link::LinkJob;
pub(in super::super) use prefix::PrefixDecoration;

#[derive(Eq, PartialEq)]
struct VisibleSpan {
    whole: Range<usize>,
    body: Range<usize>,
    active: bool,
    styles: MarkdownPresentationStyles,
}

/// One actual code proof or matched emphasis span, without a directory of the block's past ranges.
pub(in crate::markdown_document_buffer) struct SpanProof {
    pub(in crate::markdown_document_buffer) whole: Range<usize>,
    pub(in crate::markdown_document_buffer) body: Range<usize>,
    pub(in crate::markdown_document_buffer) styles: MarkdownPresentationStyles,
}

pub(super) struct CodeJob {
    // Construct the larger exclusive cursor in a later quantum. Embedding it here would make one
    // 4632-byte allocation exceed 4096; accounting for its fields separately would not split that work.
    cursor: Option<Box<InlineCodeCursor>>,
    cursor_initialized: bool,
    pending_storage: Option<InlineLinkStorageRequest>,
    reference: Option<Box<ReferenceLabel>>,
    reference_collected: bool,
    references_attached: bool,
    queries: Option<MarkdownTableReferenceQueries>,
    incoming_end: Option<(Range<usize>, Range<usize>)>,
    revision: u64,
    source_end: usize,
    pending: Option<SpanProof>,
    coordinates: Option<Coordinates>,
    spans: Vec<VisibleSpan>,
    coverage: Range<usize>,
    requested: Range<usize>,
    pub(super) output: Vec<Option<MarkdownPresentationRun>>,
    pub(super) general: bool,
    done: bool,
    emphasis: Option<Box<EmphasisJob>>,
    incoming: Option<VerifiedInlineEmphasisRun>,
    incoming_link: Option<VerifiedInlineLink>,
    link: Option<Box<LinkJob>>,
    pub(super) links: Vec<MarkdownSourceLink>,
    target_bytes: usize,
    options: pulldown_cmark::Options,
    // The seed defines the outer emphasis scope. A link label temporarily supplies its own inner
    // scope; viewport edges never substitute for syntax boundaries.
    source_range: Range<usize>,
    visible: Option<VisibleSpan>,
    visible_index: usize,
    query: Option<MarkdownPresentationQuery>,
    query_index: usize,
    query_cached: Option<MarkdownPresentationQuery>,
    containers: MarkdownPresentationStyles,
    prefix: Option<PrefixDecoration>,
    // Advance through the same source page in order instead of rescanning all prior origins per prefix.
    prefix_record_index: usize,
}

impl CodeJob {
    /// Accept the original supported paragraph/cell seed. Setext markers and container prefixes still
    /// require their own origin facts; constructing this owner does not grant plain-text authority.
    pub(super) fn new(
        block: &OrdinaryBlockRecord,
        coverage: Range<usize>,
        requested: Range<usize>,
        quota: &MarkdownTableWindowQuota,
        budget: usize,
    ) -> Result<Option<Box<Self>>, MarkdownTableRequestError> {
        if !matches!(
            block.seed.kind(),
            InlineBlockKind::Paragraph | InlineBlockKind::TableCell
        ) {
            return Ok(None);
        }
        if std::mem::size_of::<Self>() > budget {
            return Err(MarkdownTableRequestError::WorkLimit);
        }
        quota
            .admit_metadata(std::mem::size_of::<Self>())
            .map_err(MarkdownTableRequestError::Window)?;
        Ok(Some(Box::new(Self {
            cursor: None,
            cursor_initialized: false,
            pending_storage: None,
            reference: None,
            reference_collected: false,
            references_attached: false,
            queries: None,
            incoming_end: None,
            revision: block.seed.source_revision(),
            source_end: block.seed.source_end(),
            pending: None,
            coordinates: None,
            spans: Vec::new(),
            coverage,
            requested,
            output: Vec::new(),
            general: false,
            done: false,
            emphasis: None,
            incoming: None,
            incoming_link: None,
            link: None,
            links: Vec::new(),
            target_bytes: 0,
            options: block.seed.options(),
            source_range: block.seed.source_range(),
            visible: None,
            visible_index: 0,
            query: None,
            query_index: 0,
            query_cached: None,
            containers: MarkdownPresentationStyles::default(),
            prefix: None,
            prefix_record_index: 0,
        })))
    }

    pub(super) fn preparing(&self) -> bool {
        !self.cursor_initialized
    }

    /// Borrow the parent's actual block/origin only to construct this cursor, without retaining a
    /// second seed. The small job and larger cursor each have one complete allocation/admission;
    /// neither an empty yield nor amortized field accounting pretends to split an allocation.
    pub(super) fn prepare_cursor(
        &mut self,
        block: &OrdinaryBlockRecord,
        origin: Option<&BlockOriginLine>,
        limits: (usize, usize),
        quota: &MarkdownTableWindowQuota,
        budget: usize,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let metadata = std::mem::size_of::<InlineCodeCursor>();
        if metadata > budget {
            return Err(MarkdownTableRequestError::WorkLimit);
        }
        let limits = InlinePlainLimits {
            max_byte_work: limits.0,
            max_transitions: limits.1,
        };
        let cursor = match origin {
            Some(origin) => block
                .seed
                .emphasis_origin_cursor(origin, self.coverage.end, limits),
            None => block.seed.emphasis_cursor(self.coverage.end, limits),
        }
        .map_err(MarkdownTableRequestError::Global)?;
        let metadata = if let Some(cursor) = cursor {
            quota
                .admit_metadata(metadata)
                .map_err(MarkdownTableRequestError::Window)?;
            self.cursor = Some(Box::new(cursor));
            metadata
        } else {
            // Preserve the factory's actual unsupported result as General. Ending this child does
            // not authorize a default Ready publication.
            self.general = true;
            self.done = true;
            0
        };
        self.cursor_initialized = true;
        Ok(SemanticWork {
            metadata_bytes: metadata,
            structural_transitions: PLAIN_CONTROL,
            ..Default::default()
        })
    }

    /// Probe beyond the viewport for a real closer, then convert only a proven span's visible
    /// endpoints to UTF-16 through the original source tree.
    pub(super) fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        selection: &Range<usize>,
        quota: &MarkdownTableWindowQuota,
        budget: usize,
        underline: &mut MarkdownTableUnderlineRequest,
        global: Option<&mut GlobalFrontier>,
    ) -> Result<(SemanticWork, bool), MarkdownTableRequestError> {
        document
            .original_backing_health()
            .map_err(MarkdownTableRequestError::Source)?;
        if self.revision != document.generation() || self.source_end != document.byte_len() {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        if let Some(request) = self.pending_storage.as_ref() {
            let result = underline
                .stack
                .storage(request, quota, budget - PLAIN_CONTROL);
            document
                .original_backing_health()
                .map_err(MarkdownTableRequestError::Source)?;
            let (step, metadata) = result?;
            let completed = matches!(&step, InlineLinkStorageStep::Ready { .. });
            let work = self
                .cursor
                .as_mut()
                .ok_or(MarkdownTableRequestError::InvalidRequest)?
                .accept_storage(
                    request,
                    step,
                    GlobalSourceAuthority {
                        current_revision: document.generation(),
                        cancelled: false,
                    },
                    budget - PLAIN_CONTROL,
                )
                .map_err(MarkdownTableRequestError::Global)?;
            if completed {
                self.pending_storage = None;
            }
            return Ok((
                SemanticWork {
                    structural_transitions: work.transitions + PLAIN_CONTROL,
                    metadata_bytes: metadata,
                    ..Default::default()
                },
                false,
            ));
        }
        if self.reference.is_some() {
            return self
                .advance_reference(document, quota, budget, underline, global)
                .map(|work| (work, false));
        }
        if let Some((source, label)) = self.incoming_end.take() {
            let work = self
                .emphasis
                .as_mut()
                .ok_or(MarkdownTableRequestError::InvalidRequest)?
                .leave_link(&source, &label, budget, &mut underline.stack)?;
            return Ok((work, false));
        }
        if self.done {
            return Ok((SemanticWork::default(), true));
        }
        if let Some(span) = self.visible.as_ref() {
            let mut work = SemanticWork {
                structural_transitions: PLAIN_CONTROL,
                ..Default::default()
            };
            while self.visible_index < self.spans.len()
                && work.structural_transitions + std::mem::size_of::<VisibleSpan>() <= budget
            {
                work.structural_transitions += std::mem::size_of::<VisibleSpan>();
                if &self.spans[self.visible_index] == span {
                    self.visible = None;
                    return Ok((work, false));
                }
                self.visible_index += 1;
            }
            if self.visible_index == self.spans.len() {
                quota
                    .admit_metadata(std::mem::size_of::<VisibleSpan>())
                    .map_err(MarkdownTableRequestError::Window)?;
                self.spans.reserve_exact(1);
                self.spans.push(self.visible.take().unwrap());
                work.metadata_bytes += std::mem::size_of::<VisibleSpan>();
            }
            return Ok((work, false));
        }
        if let Some(proof) = self.pending.as_ref() {
            if let Some(coordinates) = self.coordinates.as_mut() {
                let (work, result) = coordinates.advance(document, budget)?;
                if let Some(values) = result {
                    let span = VisibleSpan {
                        whole: values[0]..values[3],
                        body: values[1]..values[2],
                        active: markdown_selection_intersects(selection, &proof.whole),
                        styles: proof.styles,
                    };
                    // Deduplicate in subsequent bounded quanta; do not hide a complete Vec scan
                    // inside coordinate completion.
                    self.visible = Some(span);
                    self.visible_index = 0;
                    self.coordinates = None;
                    self.pending = None;
                }
                return Ok((work, false));
            }
            let whole = &proof.whole;
            let body = &proof.body;
            let clamp = |at: usize| at.clamp(self.coverage.start, self.coverage.end);
            // Clamp distant delimiters to the visible intersection before AVL coordinate lookup;
            // never decode the entire giant code body between those endpoints.
            let (coordinates, work) = Coordinates::new(
                document,
                [
                    clamp(whole.start),
                    clamp(body.start),
                    clamp(body.end),
                    clamp(whole.end),
                ],
                budget,
            )?;
            self.coordinates = Some(coordinates);
            return Ok((work, false));
        }
        if let Some(prefix) = self.prefix.as_mut() {
            // This streaming kernel consumes proven Quote/List/Item prefixes. Footnotes use the
            // finite-block path with their real opening facts instead.
            let (work, marker, done) = prefix.advance(document, budget, &[])?;
            self.pending = marker;
            if done {
                self.prefix = None;
            }
            return Ok((work, false));
        }
        if let Some(proof) = self.incoming_link.take() {
            if self.emphasis.is_none() {
                quota
                    .admit_metadata(std::mem::size_of::<EmphasisJob>())
                    .map_err(MarkdownTableRequestError::Window)?;
                self.emphasis = Some(Box::new(EmphasisJob::new(
                    self.options,
                    self.source_range.clone(),
                )));
                self.incoming_link = Some(proof);
                return Ok((
                    SemanticWork {
                        metadata_bytes: std::mem::size_of::<EmphasisJob>(),
                        structural_transitions: PLAIN_CONTROL,
                        ..Default::default()
                    },
                    false,
                ));
            }
            let (work, entered) = self.emphasis.as_mut().unwrap().enter_link(
                &proof,
                quota,
                budget,
                &mut underline.stack,
            )?;
            if !entered {
                self.incoming_link = Some(proof);
                return Ok((work, false));
            }
            let source = proof.source_range();
            let metadata = if source.start < self.coverage.end && self.coverage.start < source.end {
                quota
                    .admit_metadata(std::mem::size_of::<LinkJob>())
                    .map_err(MarkdownTableRequestError::Window)?;
                self.link = Some(Box::new(LinkJob::new(
                    proof,
                    crate::markdown_viewport_presentation::table_window::TABLE_WINDOW_MAX_REFERENCE_BYTES
                        .saturating_sub(self.target_bytes),
                )));
                std::mem::size_of::<LinkJob>()
            } else {
                0 // Offscreen labels still isolate emphasis, without copying a URL or retaining a click target.
            };
            return Ok((
                SemanticWork {
                    metadata_bytes: work.metadata_bytes + metadata,
                    ..work
                },
                false,
            ));
        }
        if let Some(link) = self.link.as_mut() {
            let (mut work, projection) =
                link.advance(document, selection, quota, budget, self.queries.as_ref())?;
            if let Some(projection) = projection {
                self.link = None;
                if let Some(link) = projection.link {
                    quota
                        .admit_metadata(std::mem::size_of::<MarkdownSourceLink>())
                        .map_err(MarkdownTableRequestError::Window)?;
                    self.target_bytes += link.destination.byte_len();
                    self.links.reserve_exact(1);
                    self.links.push(link);
                    work.metadata_bytes += std::mem::size_of::<MarkdownSourceLink>();
                }
                self.visible = Some(projection.span);
                self.visible_index = 0;
            }
            return Ok((work, false));
        }
        if let Some(run) = self.incoming.take() {
            if self.emphasis.is_none() {
                quota
                    .admit_metadata(std::mem::size_of::<EmphasisJob>())
                    .map_err(MarkdownTableRequestError::Window)?;
                self.emphasis = Some(Box::new(EmphasisJob::new(
                    self.options,
                    self.source_range.clone(),
                )));
                self.incoming = Some(run);
                return Ok((
                    SemanticWork {
                        metadata_bytes: std::mem::size_of::<EmphasisJob>(),
                        structural_transitions: PLAIN_CONTROL,
                        ..Default::default()
                    },
                    false,
                ));
            }
            self.emphasis.as_mut().unwrap().start(run)?;
            return Ok((
                SemanticWork {
                    structural_transitions: PLAIN_CONTROL,
                    ..Default::default()
                },
                false,
            ));
        }
        if let Some(emphasis) = self.emphasis.as_mut()
            && emphasis.busy()
        {
            let (work, span) = emphasis.advance(quota, budget, &mut underline.stack)?;
            if let Some(span) = span
                && span.whole.start < self.coverage.end
                && self.coverage.start < span.whole.end
            {
                self.pending = Some(span);
            }
            return Ok((work, false));
        }
        if self.cursor.is_none() {
            if let Some(mut emphasis) = self.emphasis.take() {
                emphasis.finish();
            } // Only finish the algorithm here; the parent closes the shared file after its last underline consumer.
            self.done = true;
            return Ok((
                SemanticWork {
                    structural_transitions: PLAIN_CONTROL,
                    ..Default::default()
                },
                true,
            ));
        }
        let tree = tree_height(document);
        let child = budget.saturating_sub(tree + PLAIN_CONTROL) * 4 / 5;
        if child < 2048 {
            return Err(MarkdownTableRequestError::WorkLimit);
        }
        let cursor = self.cursor.as_mut().unwrap();
        let frame = document
            .table_source_frame(cursor.next_byte(), child)
            .map_err(MarkdownTableRequestError::Source)?;
        let source_bytes = frame.bytes.len();
        let workspace_before = cursor.allocated_workspace_bytes();
        let step = cursor
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
        frame.finish().map_err(MarkdownTableRequestError::Source)?;
        let step = step?;
        let mut allocated = cursor.allocated_workspace_bytes() - workspace_before;
        quota
            .admit_metadata(allocated)
            .map_err(MarkdownTableRequestError::Window)?;
        let measured = match &step {
            InlineCodeStep::Progress { work }
            | InlineCodeStep::NeedInput { work }
            | InlineCodeStep::Span { work, .. }
            | InlineCodeStep::Prefix { work, .. }
            | InlineCodeStep::Emphasis { work, .. }
            | InlineCodeStep::Link { work, .. }
            | InlineCodeStep::LinkEnd { work, .. }
            | InlineCodeStep::Storage { work, .. }
            | InlineCodeStep::Reference { work, .. }
            | InlineCodeStep::Proven { work, .. }
            | InlineCodeStep::NeedsGeneralInline { work } => *work,
        };
        match step {
            InlineCodeStep::Span { proof, .. } => {
                let range = proof.source_range();
                if range.start < self.coverage.end && self.coverage.start < range.end {
                    self.pending = Some(SpanProof {
                        whole: range,
                        body: proof.raw_body_range(),
                        styles: MarkdownPresentationStyles::INLINE_CODE,
                    });
                }
            }
            InlineCodeStep::Prefix { proof, .. } => {
                let range = proof.source_range();
                if range.start < self.coverage.end && self.coverage.start < range.end {
                    self.prefix = Some(PrefixDecoration::new(proof));
                } // The original SourceRequest supplies the physical-line owner; clipped coverage does not decide visibility.
            }
            InlineCodeStep::Emphasis { run, .. } => self.incoming = Some(run),
            InlineCodeStep::Link { proof, .. } => self.incoming_link = Some(proof),
            InlineCodeStep::LinkEnd { source, label, .. } => {
                self.incoming_end = Some((source, label));
            }
            InlineCodeStep::Storage { request, .. } => self.pending_storage = Some(request),
            InlineCodeStep::Reference { request, .. } => {
                quota
                    .admit_metadata(std::mem::size_of::<ReferenceLabel>())
                    .map_err(MarkdownTableRequestError::Window)?;
                allocated += std::mem::size_of::<ReferenceLabel>();
                self.reference = Some(Box::new(ReferenceLabel::new(request)));
                self.reference_collected = false;
            }
            InlineCodeStep::Proven {
                requires_general_styles,
                ..
            } => {
                self.general = requires_general_styles;
                let containers = self.cursor.as_ref().unwrap().containers();
                for (present, style) in [
                    (containers.quote, MarkdownPresentationStyles::BLOCK_QUOTE),
                    (
                        containers.unordered_list,
                        MarkdownPresentationStyles::UNORDERED_LIST,
                    ),
                    (
                        containers.ordered_list,
                        MarkdownPresentationStyles::ORDERED_LIST,
                    ),
                ] {
                    if present {
                        self.containers = self.containers.union(style);
                    }
                } // Publish inherited styles only after the original scope and inline proof complete, without guessing ancestors from marker spelling.
                self.cursor = None;
            }
            InlineCodeStep::NeedsGeneralInline { .. } => {
                self.general = true;
                self.cursor = None;
            }
            _ => {}
        }
        Ok((
            SemanticWork {
                // Storage/reference events can pause at a frame prefix. Charge the entire actual
                // borrowed frame, including its unread tail, rather than omitting that acquisition.
                source_bytes,
                metadata_bytes: allocated,
                structural_transitions: measured.transitions + tree + PLAIN_CONTROL,
                source_tree_steps: tree,
                ..Default::default()
            },
            self.done,
        ))
    }

    /// Move the unique Queries while the original cursor is paused, without copying its map.
    /// Resume only after the same Global resolves the first definition or proves true EOF.
    fn advance_reference(
        &mut self,
        document: &MarkdownDocumentBuffer,
        quota: &MarkdownTableWindowQuota,
        budget: usize,
        underline: &mut MarkdownTableUnderlineRequest,
        global: Option<&mut GlobalFrontier>,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let reference = self.reference.as_mut().unwrap();
        if reference.label().is_none() {
            return reference.advance(document, quota, budget);
        }
        if !self.reference_collected {
            if self.queries.is_none() {
                self.queries = if underline
                    .ready_pairs_for(document)
                    .map_err(MarkdownTableRequestError::Window)?
                    .is_some()
                {
                    underline.take_resolved_references()?
                } else {
                    None
                };
                if self.queries.is_none() {
                    let mut queries = MarkdownTableReferenceQueries::for_batch(Some(quota.clone()));
                    queries.seal_batch();
                    self.queries = Some(queries);
                }
            }
            let label = reference.label().unwrap();
            let before = quota.metadata_used();
            self.queries
                .as_mut()
                .unwrap()
                .collect_verified_label(label)
                .map_err(MarkdownTableRequestError::Window)?;
            self.reference_collected = true;
            return Ok(SemanticWork {
                structural_transitions: PLAIN_CONTROL,
                normalizer_input_bytes: label.len(),
                metadata_bytes: quota.metadata_used() - before,
                ..Default::default()
            });
        }
        if self.references_attached {
            if underline
                .ready_pairs_for(document)
                .map_err(MarkdownTableRequestError::Window)?
                .is_some()
            {
                self.queries = underline.take_resolved_references()?;
                self.references_attached = false;
                return Ok(SemanticWork {
                    structural_transitions: PLAIN_CONTROL,
                    ..Default::default()
                });
            }
            return underline
                .advance(
                    document,
                    global.ok_or(MarkdownTableRequestError::InvalidRequest)?,
                    budget - PLAIN_CONTROL,
                )
                .map(|work| {
                    let mut work = underline_work(work);
                    work.structural_transitions += PLAIN_CONTROL;
                    work
                });
        }
        let queries = self
            .queries
            .as_ref()
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        if !queries.is_ready() {
            let allocated = underline.resolve_references(self.queries.take().unwrap())?;
            self.references_attached = true;
            return Ok(SemanticWork {
                structural_transitions: PLAIN_CONTROL,
                metadata_bytes: allocated,
                ..Default::default()
            });
        }
        let outcome = if queries
            .resolve_for_classification(reference.label().unwrap())
            .map_err(MarkdownTableRequestError::Window)?
            .is_some()
        {
            InlineReferenceResolution::Resolved
        } else {
            InlineReferenceResolution::Missing
        };
        let work = self
            .cursor
            .as_mut()
            .ok_or(MarkdownTableRequestError::InvalidRequest)?
            .accept_reference(
                &reference.request,
                outcome,
                GlobalSourceAuthority {
                    current_revision: document.generation(),
                    cancelled: false,
                },
            )
            .map_err(MarkdownTableRequestError::Global)?;
        self.reference = None;
        self.reference_collected = false;
        Ok(SemanticWork {
            structural_transitions: work.transitions + PLAIN_CONTROL,
            ..Default::default()
        })
    }

    /// Query proven spans for this viewport. Unmatched intervals obtain plain/container styles only
    /// after the original plain/code prefix has completed successfully.
    pub(super) fn advance_query(
        &mut self,
        index: usize,
        budget: usize,
    ) -> (SemanticWork, Option<MarkdownPresentationQuery>) {
        let mut work = SemanticWork {
            structural_transitions: PLAIN_CONTROL,
            ..Default::default()
        };
        if let Some(cached) = self.query_cached.as_ref()
            && cached.utf16_range.contains(&index)
        {
            return (
                work,
                Some(MarkdownPresentationQuery {
                    utf16_range: index..cached.utf16_range.end,
                    styles: cached.styles,
                }),
            );
        }
        if self.query.is_none() {
            self.query = Some(MarkdownPresentationQuery {
                utf16_range: index..self.requested.end,
                styles: self.containers,
            });
            self.query_index = 0;
        }
        let query = self.query.as_mut().unwrap();
        while self.query_index < self.spans.len()
            && work.structural_transitions + std::mem::size_of::<VisibleSpan>() <= budget
        {
            let span = &self.spans[self.query_index];
            self.query_index += 1;
            work.structural_transitions += std::mem::size_of::<VisibleSpan>();
            let end = &mut query.utf16_range.end;
            let styles = &mut query.styles;
            if index < span.whole.start {
                *end = (*end).min(span.whole.start);
                continue;
            }
            if span.whole.contains(&index) {
                *styles = styles.union(span.styles);
                *end = (*end).min(span.whole.end);
                if index < span.body.start {
                    *end = (*end).min(span.body.start);
                } else if index < span.body.end {
                    *end = (*end).min(span.body.end);
                }
                if !span.body.contains(&index) {
                    *styles = styles.union(MarkdownPresentationStyles::MARKER);
                    if !span.active {
                        *styles = styles.union(MarkdownPresentationStyles::HIDDEN_MARKER);
                    }
                }
            }
        }
        if self.query_index != self.spans.len() {
            return (work, None);
        }
        let query = self.query.take().unwrap();
        self.query_cached = Some(query.clone());
        (work, Some(query))
    }
    pub(super) fn push_output(
        &mut self,
        run: MarkdownPresentationRun,
        quota: &MarkdownTableWindowQuota,
    ) -> Result<(), MarkdownTableRequestError> {
        quota
            .admit_metadata(std::mem::size_of::<Option<MarkdownPresentationRun>>())
            .map_err(MarkdownTableRequestError::Window)?;
        self.output.reserve_exact(1);
        self.output.push(Some(run));
        Ok(())
    }

    /// Move a completed real target to the parent's existing handoff without another String copy.
    pub(super) fn take_link(&mut self) -> Option<MarkdownSourceLink> {
        self.links.pop()
    }
    /// Return proof, coordinate, parser, and lookup scratch before publication; retain sealed output
    /// runs and targets already awaiting the parent's ownership transfer.
    pub(super) fn release_scratch(&mut self) {
        self.spans = Vec::new();
        self.pending = None;
        self.coordinates = None;
        self.cursor = None;
        self.pending_storage = None;
        self.reference = None;
        self.reference_collected = false;
        self.references_attached = false;
        self.queries = None;
        self.emphasis = None;
        self.incoming = None;
        self.incoming_link = None;
        self.incoming_end = None;
        self.link = None;
        self.visible = None;
        self.query = None;
        self.query_cached = None;
        self.prefix = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Observe this process's actual descriptor inode without acquiring close ownership. Parallel
    /// tests may reuse the descriptor number for a different file after the original owner closes it.
    pub(super) fn descriptor_identity(
        descriptor: std::os::fd::RawFd,
    ) -> Option<(libc::dev_t, libc::ino_t)> {
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: fstat writes a complete stat into valid stack storage; it creates no File or close owner.
        if unsafe { libc::fstat(descriptor, status.as_mut_ptr()) } == 0 {
            // SAFETY: a successful fstat initialized the complete structure before this read.
            let status = unsafe { status.assume_init() };
            Some((status.st_dev, status.st_ino))
        } else {
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EBADF)
            );
            None
        }
    }

    #[test]
    /// After actual stack paging, cancellation, source EIO, and index EBADF must preserve their first
    /// failure and close the unique file descriptor.
    fn active_emphasis_file_is_closed_on_cancel_and_real_source_or_storage_errors() {
        for scenario in ["cancel", "source", "storage"] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("giant-emphasis.md");
            let source = format!(
                "{}{}{}\n",
                "*a ".repeat(200),
                // Exceed the original 64-KiB resident slot so reclaim really executes msync; merely
                // arming an injection on a short file would not exercise source-failure handling.
                "中".repeat(40_000),
                " a*".repeat(200)
            );
            std::fs::write(&path, &source).unwrap();
            let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap();
            let observer = document.original_backing_observer().unwrap();
            let authority = MarkdownViewportSourceAuthority {
                context_revision: 57,
                selection: document.selection(),
                cancelled: false,
            };
            let mut request = document
                .viewport_leaf_source_request(1000..1040, 57, document.selection())
                .unwrap();
            let descriptor = loop {
                request.advance(&document, authority, 4096).unwrap();
                if let Some(descriptor) = request
                    .semantic
                    .as_ref()
                    .and_then(|job| job.underline.as_ref())
                    .and_then(|job| job.stack.descriptor())
                {
                    break descriptor;
                }
                assert!(request.sources_for(&document, authority).unwrap().is_none());
            };
            let descriptor = if scenario == "storage" {
                let read_only = directory.path().join("read-only-index");
                std::fs::write(&read_only, b"unchanged").unwrap();
                let stack = &mut request
                    .semantic
                    .as_mut()
                    .unwrap()
                    .underline
                    .as_mut()
                    .unwrap()
                    .stack;
                stack.replace_file(std::fs::File::open(read_only).unwrap());
                stack.descriptor().unwrap()
            } else {
                descriptor
            };
            let original_inode = descriptor_identity(descriptor).unwrap();
            if scenario == "cancel" {
                request.cancel();
            } else if scenario == "source" {
                observer.fail_next_reclaim_for_test();
            }
            let error = loop {
                match request.advance(&document, authority, 4096) {
                    Ok(_) => assert!(request.sources_for(&document, authority).unwrap().is_none()),
                    Err(error) => break error,
                }
            };
            match scenario {
                "cancel" => assert_eq!(error, MarkdownTableRequestError::Cancelled),
                "source" => assert_eq!(
                    error,
                    MarkdownTableRequestError::Source(
                        MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
                    )
                ),
                _ => assert_eq!(
                    error,
                    MarkdownTableRequestError::InlineStorageUnavailable(libc::EBADF)
                ),
            }
            assert_eq!(
                request.advance(&document, authority, 4096).err(),
                Some(error)
            );
            // The old descriptor must be closed or reused for another inode; the same inode would
            // mean the actual resource is still retained.
            assert_ne!(descriptor_identity(descriptor), Some(original_inode));
            assert!(request.semantic.is_none());
            drop(request);
            drop(document);
            assert_eq!(observer.mapped_bytes(), 0);
            assert_eq!(observer.managed_resident_bytes(), 0);
        }
    }

    #[test]
    /// Cancelling or failing a giant probe before its distant closer must publish no partial styles
    /// and must not retain the document mapping.
    fn active_giant_code_probe_releases_source_and_owner_on_cancel_and_reclaim_error() {
        for cancel in [true, false] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("code-resource.md");
            let source = format!("prefix ` {} ` tail\n", "中".repeat(40_000));
            std::fs::write(&path, &source).unwrap();
            let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap();
            let observer = document.original_backing_observer().unwrap();
            let authority = MarkdownViewportSourceAuthority {
                context_revision: 57,
                selection: document.selection(),
                cancelled: false,
            };
            let mut request = document
                .viewport_leaf_source_request(100..120, 57, document.selection())
                .unwrap();
            for _ in 0..source.len() * 100 + 10000 {
                request.advance(&document, authority, 4096).unwrap();
                if request
                    .semantic
                    .as_ref()
                    .and_then(|job| job.code.as_ref())
                    .and_then(|job| job.cursor.as_ref())
                    .is_some_and(|cursor| cursor.next_byte() > 20_000)
                {
                    break;
                }
            }
            assert!(
                request
                    .semantic
                    .as_ref()
                    .and_then(|job| job.code.as_ref())
                    .and_then(|job| job.cursor.as_ref())
                    .is_some_and(|cursor| cursor.next_byte() > 20_000),
                "必须先进入真实 code lookahead 再注入失败"
            );
            if cancel {
                request.cancel();
                assert_eq!(
                    request.advance(&document, authority, 4096).err(),
                    Some(MarkdownTableRequestError::Cancelled)
                );
                assert_eq!(observer.first_errno(), None);
                assert!(document.original_backing_health().is_ok());
            } else {
                observer.fail_next_reclaim_for_test();
                let error = loop {
                    match request.advance(&document, authority, 4096) {
                        Ok(MarkdownViewportSourceStep::Progress { .. }) => {}
                        Ok(_) => panic!("未完成真实 code 回收不能取得 Ready"),
                        Err(error) => break error,
                    }
                };
                assert_eq!(
                    error,
                    MarkdownTableRequestError::Source(
                        crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
                    )
                );
                assert_eq!(
                    request.advance(&document, authority, 4096).err(),
                    Some(error)
                );
                assert_eq!(observer.first_errno(), Some(libc::EIO));
            }
            assert!(request.semantic.is_none() && request.leaf_semantic.is_none());
            assert!(request.batch.source_global_mut().is_err());
            assert_eq!(request.total_work().copied_source_bytes, 0);
            assert_eq!(request.total_work().parser_input_bytes, 0);
            drop(request);
            drop(document);
            assert_eq!(observer.mapped_bytes(), 0);
            assert_eq!(observer.managed_resident_bytes(), 0);
        }
    }
}
