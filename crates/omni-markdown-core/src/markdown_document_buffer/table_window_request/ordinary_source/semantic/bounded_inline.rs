//! Project complete inline events only for actual closed blocks of at most 2048 bytes, preserving Root, cell, and ATX entry authority.

use super::super::leaf_semantic::scoped::ScopedLeaf;
use super::*;
use crate::MarkdownSourceLink;
use crate::markdown_viewport_presentation::parser::{
    MarkdownLinkMarkers, MarkdownParserSeed, build_bounded_inline_presentation,
};
use crate::markdown_viewport_presentation::table_window::{
    MarkdownTableWindowQuota, references::MarkdownTableReferenceQueries,
};
use pulldown_cmark::global_frontier::{InlineParagraphContext, InlineParagraphStep};
use pulldown_cmark::{Event, Parser, Tag, TagEnd};

pub(super) struct BoundedInline {
    range: Range<usize>,
    options: pulldown_cmark::Options,
    source: BoundedSource,
    bytes: Vec<u8>,
    coordinates: Option<Coordinates>,
    start: Option<usize>,
    queries: Option<MarkdownTableReferenceQueries>,
    references_attached: bool,
    pub(super) parsed: Vec<MarkdownPresentationRun>,
    pub(super) output: Vec<Option<MarkdownPresentationRun>>,
    pub(super) links: Vec<MarkdownSourceLink>,
    parsed_complete: bool,
    presentation_complete: bool,
}

/// Keep closed-source kinds exclusive; the parent consumes actual ATX scope while Parser uses its original single-line heading entry.
enum BoundedSource {
    Root,
    Cell(Rc<OrdinaryBlockRecord>),
    Atx(Box<pulldown_cmark::global_frontier::VerifiedGlobalLeafBlock>),
    Scoped {
        context: Box<InlineParagraphContext>,
        prefixes: Option<Box<ScopedLeaf>>,
    },
}

impl BoundedInline {
    /// Do not drive code/HTML reference interpretation before original location, bounded copy, and query transfer finish.
    pub(super) fn references_attached(&self) -> bool {
        self.references_attached
    }

    /// Accept only the seed's actual closed bounded range; giant paragraphs never enter this whole-slice Parser entry.
    pub(super) fn new(
        cursor: &InlinePlainCursor,
        block: &Rc<OrdinaryBlockRecord>,
        quota: &MarkdownTableWindowQuota,
    ) -> Result<Option<Box<Self>>, MarkdownTableRequestError> {
        let Some(range) = cursor.bounded_inline_range() else {
            return Ok(None);
        };
        let mut bounded = Self::allocate(range, block.seed.options(), quota)?;
        if block.seed.kind() == InlineBlockKind::TableCell {
            bounded.source = BoundedSource::Cell(Rc::clone(block));
        }
        Ok(Some(bounded))
    }

    /// Derive ATX authority from an actual closed leaf with parent-verified scope, never an arbitrary line or naked range.
    pub(super) fn supports_atx(
        leaf: &pulldown_cmark::global_frontier::VerifiedGlobalLeafBlock,
    ) -> bool {
        matches!(leaf.kind(), LeafBlockKind::AtxHeading(_))
            && leaf.source_range().end - leaf.physical_start() <= 2048
    }

    /// Preserve actual physical prefixes and the complete heading end; the same Parser owns heading and inline precedence.
    pub(super) fn for_atx(
        leaf: &pulldown_cmark::global_frontier::VerifiedGlobalLeafBlock,
        quota: &MarkdownTableWindowQuota,
    ) -> Result<Box<Self>, MarkdownTableRequestError> {
        if !Self::supports_atx(leaf) {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        let mut bounded = Self::allocate(
            leaf.physical_start()..leaf.source_range().end,
            leaf.options(),
            quota,
        )?;
        quota
            .admit_metadata(std::mem::size_of_val(leaf))
            .map_err(MarkdownTableRequestError::Window)?;
        bounded.source = BoundedSource::Atx(Box::new(leaf.clone()));
        Ok(bounded)
    }

    /// Parse only original closed short scoped blocks, retaining raw physical prefixes while incrementally consuming original scope/continuation state.
    pub(super) fn supports_scoped(block: &OrdinaryBlockRecord) -> bool {
        block.seed.scope_depth() > 1
            && matches!(
                block.seed.kind(),
                InlineBlockKind::Paragraph | InlineBlockKind::SetextHeading(_)
            )
            && block.seed.physical_range().len() <= 2048
    }

    pub(super) fn for_scoped(
        block: &OrdinaryBlockRecord,
        limits: (usize, usize),
        quota: &MarkdownTableWindowQuota,
    ) -> Result<Box<Self>, MarkdownTableRequestError> {
        if !Self::supports_scoped(block) {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        quota
            .admit_metadata(std::mem::size_of::<InlineParagraphContext>())
            .map_err(MarkdownTableRequestError::Window)?;
        let context = block
            .seed
            .bounded_paragraph_context(InlinePlainLimits {
                max_byte_work: limits.0,
                max_transitions: limits.1,
            })
            .map_err(MarkdownTableRequestError::Global)?
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        let mut bounded = Self::allocate(block.seed.physical_range(), block.seed.options(), quota)?;
        bounded.source = BoundedSource::Scoped {
            context: Box::new(context),
            prefixes: None,
        };
        Ok(bounded)
    }

    pub(super) fn scoped_metadata() -> usize {
        std::mem::size_of::<Self>() + std::mem::size_of::<InlineParagraphContext>()
    }

    pub(super) fn unsupported_scope(&self) -> bool {
        matches!(&self.source, BoundedSource::Scoped { context, .. } if context.is_unsupported())
    }

    /// All source kinds share one bounded job; admit workspace here and copy text frame by frame only in later steps.
    fn allocate(
        range: Range<usize>,
        options: pulldown_cmark::Options,
        quota: &MarkdownTableWindowQuota,
    ) -> Result<Box<Self>, MarkdownTableRequestError> {
        quota
            .admit_metadata(std::mem::size_of::<Self>())
            .map_err(MarkdownTableRequestError::Window)?;
        Ok(Box::new(Self {
            bytes: Vec::new(),
            range,
            options,
            source: BoundedSource::Root,
            coordinates: None,
            start: None,
            queries: None,
            references_attached: false,
            parsed: Vec::new(),
            output: Vec::new(),
            links: Vec::new(),
            parsed_complete: false,
            presentation_complete: false,
        }))
    }

    /// Active ATX retains this original leaf ticket and bounded-job handle; the parent charges the extra Box during initial construction.
    pub(super) fn atx_metadata() -> usize {
        std::mem::size_of::<Self>()
            + std::mem::size_of::<pulldown_cmark::global_frontier::VerifiedGlobalLeafBlock>()
    }

    fn parser_seed(&self) -> Option<MarkdownParserSeed<'_>> {
        match &self.source {
            BoundedSource::Root => None,
            BoundedSource::Cell(block) => Some(MarkdownParserSeed::Cell(&block.seed)),
            BoundedSource::Atx(leaf) => Some(MarkdownParserSeed::Atx(leaf)),
            BoundedSource::Scoped { context, .. } => Some(MarkdownParserSeed::Scoped(context)),
        }
    }

    /// Each step locates, borrows one bounded frame, or runs one input-bounded original Parser; source-return failure publishes no result.
    pub(super) fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        underline: &mut Option<MarkdownTableUnderlineRequest>,
        quota: &MarkdownTableWindowQuota,
        selection: MarkdownTextSelection,
        selection_bytes: &Range<usize>,
        budget: usize,
    ) -> Result<(SemanticWork, bool), MarkdownTableRequestError> {
        if self.parsed_complete {
            return Ok((SemanticWork::default(), true));
        }
        if let BoundedSource::Scoped { context, .. } = &mut self.source
            && !context.is_complete()
            && (!context.requires_source() || self.bytes.len() == self.range.len())
        {
            // Reject unsupported scope before allocating/copying text; enter bounded source copying only when actual context needs input.
            let source = std::str::from_utf8(&self.bytes)
                .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
            let before = context.allocated_workspace_bytes();
            let step = context
                .advance(
                    source,
                    GlobalSourceAuthority {
                        current_revision: document.generation(),
                        cancelled: false,
                    },
                    budget,
                )
                .map_err(MarkdownTableRequestError::Global)?;
            let metadata = context.allocated_workspace_bytes() - before;
            quota
                .admit_metadata(metadata)
                .map_err(MarkdownTableRequestError::Window)?;
            let measured = match step {
                InlineParagraphStep::Progress { work }
                | InlineParagraphStep::Complete { work }
                | InlineParagraphStep::Unsupported { work } => work,
            };
            return Ok((
                SemanticWork {
                    parser_input_bytes: measured.byte_reads,
                    metadata_bytes: metadata,
                    structural_transitions: measured.transitions,
                    ..Default::default()
                },
                context.is_unsupported(),
            ));
        }
        if self.start.is_none() {
            // Obtain the absolute UTF-16 origin through original source coordinates, not a byte-offset approximation.
            if let Some(coordinates) = self.coordinates.as_mut() {
                let (work, result) = coordinates.advance(document, budget)?;
                if let Some(values) = result {
                    self.start = Some(values[0]);
                    self.coordinates = None;
                }
                return Ok((work, false));
            }
            let (coordinates, work) = Coordinates::new(document, [self.range.start; 4], budget)?;
            self.coordinates = Some(coordinates);
            return Ok((work, false));
        }
        if self.bytes.len() < self.range.len() {
            // Only an admitted short closed block reaches this copy. Preserve actual capacity accounting separately from bytes copied this step.
            let allocation = if self.bytes.capacity() == 0 {
                quota
                    .admit_metadata(self.range.len())
                    .map_err(MarkdownTableRequestError::Window)?;
                self.bytes.reserve_exact(self.range.len());
                quota
                    .admit_metadata(self.bytes.capacity() - self.range.len())
                    .map_err(MarkdownTableRequestError::Window)?;
                self.bytes.capacity()
            } else {
                0
            };
            let tree = tree_height(document);
            let maximum = (budget.saturating_sub(tree + PLAIN_CONTROL) / 4).max(1);
            let frame = document
                .table_source_frame(
                    self.range.start + self.bytes.len(),
                    maximum.min(self.range.len() - self.bytes.len()),
                )
                .map_err(MarkdownTableRequestError::Source)?;
            let count = frame.bytes.len().min(self.range.len() - self.bytes.len());
            self.bytes.extend_from_slice(&frame.bytes[..count]);
            frame.finish().map_err(MarkdownTableRequestError::Source)?;
            return Ok((
                SemanticWork {
                    source_bytes: count,
                    copied_source_bytes: count,
                    source_tree_steps: tree,
                    structural_transitions: count + tree + PLAIN_CONTROL,
                    metadata_bytes: allocation,
                    allocated_source_capacity: allocation,
                    ..Default::default()
                },
                false,
            ));
        }
        let source = std::str::from_utf8(&self.bytes)
            .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
        if let BoundedSource::Scoped { context, prefixes } = &mut self.source {
            if prefixes.is_none() {
                *prefixes = Some(ScopedLeaf::for_inline(
                    context,
                    self.range.clone(),
                    selection_bytes.clone(),
                    quota,
                )?);
                return Ok((
                    SemanticWork {
                        metadata_bytes: std::mem::size_of::<ScopedLeaf>(),
                        structural_transitions: PLAIN_CONTROL,
                        ..Default::default()
                    },
                    false,
                ));
            }
            let prefixes = prefixes.as_mut().unwrap();
            if !prefixes.done() {
                return prefixes
                    .advance_inline(document, context, quota, budget)
                    .map(|(work, _)| (work, false));
            }
        }
        if !self.references_attached {
            // Transfer bounded queries to the same underline/Global owner, which must close the real reference context before parsing styles.
            let queries = MarkdownTableReferenceQueries::from_bounded_inline(
                source,
                self.options,
                quota.clone(),
                self.parser_seed(),
            )
            .map_err(MarkdownTableRequestError::Window)?;
            if underline.is_none() {
                quota
                    .reserve_underline_workspace()
                    .map_err(MarkdownTableRequestError::Window)?;
                *underline = Some(
                    document
                        .table_underline_request(std::slice::from_ref(&self.range), quota.clone())
                        .map_err(MarkdownTableRequestError::Window)?,
                );
            }
            let metadata_bytes = underline.as_mut().unwrap().resolve_references(queries)?;
            self.references_attached = true;
            return Ok((
                SemanticWork {
                    metadata_bytes,
                    parser_input_bytes: source.len(),
                    structural_transitions: PLAIN_CONTROL,
                    ..Default::default()
                },
                false,
            ));
        }
        let context = underline.as_mut().unwrap();
        if context
            .ready_pairs_for(document)
            .map_err(MarkdownTableRequestError::Window)?
            .is_none()
        {
            return Ok((SemanticWork::default(), false)); // The parent drives reference/underline lookahead through the sole Global.
        }
        if self.queries.is_none() {
            self.queries = context.take_resolved_references()?;
        }
        let queries = self
            .queries
            .as_ref()
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        if !self.presentation_complete {
            // The complete bounded Parser supplies precedence and marker visibility; only its actual run allocation enters retained metadata.
            self.parsed = build_bounded_inline_presentation(
                source,
                self.start.unwrap(),
                &ordered_selection(selection),
                self.options,
                queries,
                self.parser_seed(),
            )
            .map_err(|_| {
                MarkdownTableRequestError::Window(MarkdownTableWindowError::ParserMismatch)
            })?;
            quota
                .admit_metadata(
                    self.parsed.capacity() * std::mem::size_of::<MarkdownPresentationRun>(),
                )
                .map_err(MarkdownTableRequestError::Window)?;
            self.presentation_complete = true;
            return Ok((
                SemanticWork {
                    parser_input_bytes: source.len(),
                    structural_transitions: PLAIN_CONTROL,
                    ..Default::default()
                },
                false,
            ));
        }
        self.links = collect_links(
            source,
            self.start.unwrap(),
            self.options,
            queries,
            quota,
            self.parser_seed(),
        )?;
        self.parsed_complete = true;
        Ok((
            SemanticWork {
                parser_input_bytes: source.len(),
                structural_transitions: PLAIN_CONTROL,
                ..Default::default()
            },
            true,
        ))
    }

    /// Release parse input and queries after actual output construction; exported publication retains coordinates, styles, and click targets only.
    pub(super) fn release_scratch(&mut self) {
        self.bytes = Vec::new();
        self.queries = None;
        self.parsed = Vec::new();
        self.coordinates = None;
        self.source = BoundedSource::Root; // Return bounded context/prefix metadata; publication retains only sealed output.
    }

    /// Original projection stores styled segments only; other positions in the genuinely closed parsed block are proved ordinary text, not holes.
    pub(super) fn query(&self, index: usize, requested_end: usize) -> MarkdownPresentationQuery {
        let scoped = match &self.source {
            BoundedSource::Scoped {
                prefixes: Some(prefixes),
                ..
            } => Some(prefixes.query(index, requested_end)),
            _ => None,
        };
        if let Some(run) = self
            .parsed
            .iter()
            .find(|run| run.utf16_range.contains(&index))
        {
            return MarkdownPresentationQuery {
                utf16_range: index..scoped.as_ref().map_or(run.utf16_range.end, |query| {
                    query.utf16_range.end.min(run.utf16_range.end)
                }),
                styles: scoped
                    .as_ref()
                    .map_or(run.styles, |query| run.styles.union(query.styles)),
            };
        }
        let end = self
            .parsed
            .iter()
            .find(|run| run.utf16_range.start > index)
            .map_or(requested_end, |run| run.utf16_range.start);
        MarkdownPresentationQuery {
            utf16_range: index
                ..scoped
                    .as_ref()
                    .map_or(end, |query| end.min(query.utf16_range.end)),
            styles: scoped.map_or(MarkdownPresentationStyles::default(), |query| query.styles),
        }
    }

    /// Admit one output slot before requesting single-slot growth; this logical metadata charge is not allocator overhead or RSS.
    pub(super) fn push_output(
        &mut self,
        run: MarkdownPresentationRun,
        quota: &MarkdownTableWindowQuota,
    ) -> Result<(), MarkdownTableRequestError> {
        if self.output.len() == self.output.capacity() {
            quota
                .admit_metadata(std::mem::size_of::<Option<MarkdownPresentationRun>>())
                .map_err(MarkdownTableRequestError::Window)?;
            self.output.reserve_exact(1);
        }
        self.output.push(Some(run));
        Ok(())
    }
}

struct ActiveLink {
    destination: crate::MarkdownLinkDestination,
    source: MarkdownLinkMarkers,
}

/// Use original Parser destinations and actual child-event label ranges rather than guessing a closing bracket with find(']').
fn collect_links(
    source: &str,
    absolute_start: usize,
    options: pulldown_cmark::Options,
    queries: &MarkdownTableReferenceQueries,
    quota: &MarkdownTableWindowQuota,
    seed: Option<MarkdownParserSeed<'_>>,
) -> Result<Vec<MarkdownSourceLink>, MarkdownTableRequestError> {
    let mut callback = |link: pulldown_cmark::BrokenLink<'_>| {
        queries
            .resolve(&link.reference)
            .map(|(destination, title)| (destination.into(), title.into()))
    };
    let parser = match seed {
        Some(MarkdownParserSeed::Cell(seed)) => {
            Parser::new_with_table_cell_seed(source, seed, Some(&mut callback)).map_err(|_| {
                MarkdownTableRequestError::Window(MarkdownTableWindowError::ParserMismatch)
            })?
        }
        Some(MarkdownParserSeed::Atx(leaf)) => {
            Parser::new_with_atx_leaf_seed(source, leaf, Some(&mut callback)).map_err(|_| {
                MarkdownTableRequestError::Window(MarkdownTableWindowError::ParserMismatch)
            })?
        }
        Some(MarkdownParserSeed::Scoped(context)) => {
            Parser::new_with_inline_block_seed(source, context, Some(&mut callback)).map_err(
                |_| MarkdownTableRequestError::Window(MarkdownTableWindowError::ParserMismatch),
            )?
        }
        None => Parser::new_with_broken_link_callback(source, options, Some(&mut callback)),
    };
    let mut active: Vec<ActiveLink> = Vec::new();
    let mut links = Vec::new();
    let mut target_bytes = 0usize;
    for (event, range) in parser.into_offset_iter() {
        if !matches!(event, Event::End(TagEnd::Link)) {
            for link in &mut active {
                link.source.observe(&range);
            }
        } // A link's own End includes URL/title ownership and cannot enlarge its label; actual child events retain their complete ranges.
        match event {
            Event::Start(Tag::Link {
                dest_url,
                link_type,
                id,
                ..
            }) => {
                target_bytes = target_bytes
                    .checked_add(dest_url.len())
                    .ok_or(MarkdownTableRequestError::WorkLimit)?;
                if target_bytes > 16 * 1024 {
                    return Err(MarkdownTableRequestError::Window(
                        MarkdownTableWindowError::ReferenceLimit,
                    ));
                }
                // Unknown types are references resolved by the original broken-link callback through the sole Queries.
                // Their targets can exceed this short block, so share the original Arc instead of copying up to 16 KiB with into_string.
                // Direct links and in-block definitions retain this Parser's actual value rather than replacing it with another query result.
                let reference = matches!(
                    link_type,
                    pulldown_cmark::LinkType::ReferenceUnknown
                        | pulldown_cmark::LinkType::CollapsedUnknown
                        | pulldown_cmark::LinkType::ShortcutUnknown
                );
                let metadata = std::mem::size_of::<ActiveLink>()
                    + if reference {
                        0
                    } else {
                        dest_url.len() + crate::MarkdownLinkDestination::whole_metadata()
                    };
                quota
                    .admit_metadata(metadata)
                    .map_err(MarkdownTableRequestError::Window)?;
                let destination = if reference {
                    queries
                        .resolved_destination(&id)
                        .map_err(MarkdownTableRequestError::Window)?
                        .ok_or(MarkdownTableRequestError::Window(
                            MarkdownTableWindowError::ParserMismatch,
                        ))?
                } else {
                    crate::MarkdownLinkDestination::from_string(dest_url.into_string())
                };
                if active.len() == active.capacity() {
                    active.reserve_exact(1); // Request one additional original-event stack slot after admitting its logical metadata.
                }
                active.push(ActiveLink {
                    destination,
                    source: MarkdownLinkMarkers::new(source, range).map_err(|_| {
                        MarkdownTableRequestError::Window(MarkdownTableWindowError::ParserMismatch)
                    })?,
                });
            }
            Event::End(TagEnd::Link) => {
                let link = active
                    .pop()
                    .ok_or(MarkdownTableRequestError::InvalidRequest)?;
                let range = link.source.label_range(source, &range).map_err(|_| {
                    MarkdownTableRequestError::Window(MarkdownTableWindowError::ParserMismatch)
                })?;
                if !range.is_empty() {
                    let start = absolute_start + source[..range.start].encode_utf16().count();
                    let end = start + source[range].encode_utf16().count();
                    quota
                        .admit_metadata(std::mem::size_of::<MarkdownSourceLink>())
                        .map_err(MarkdownTableRequestError::Window)?;
                    links.reserve_exact(1);
                    links.push(MarkdownSourceLink {
                        label_utf16_range: start..end,
                        destination: link.destination,
                    });
                }
            }
            _ => {} // Other containers grant no link authority; actual text children still follow original event order.
        }
    }
    Ok(links)
}
