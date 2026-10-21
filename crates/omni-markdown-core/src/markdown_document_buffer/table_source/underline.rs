//! Pair underline on one bounded source window, temporarily replaying and restoring the same structural Global.

use super::inline_code::{InlinePermission, ReferenceInput};
use super::inline_stack::StackFile;
use super::*;
use crate::markdown_document_buffer::table_reference_request;
use crate::markdown_document_buffer::table_window_request::MarkdownTableRequestError as Error;
use crate::markdown_viewport_presentation::table_window::MarkdownTableWindowQuota;
use crate::markdown_viewport_presentation::table_window::context::{
    MarkdownTableContextFrame, MarkdownTableUnderlineContext,
};
use crate::markdown_viewport_presentation::table_window::references::MarkdownTableReferenceQueries;
use pulldown_cmark::block_frontier::SourceFrame;
use pulldown_cmark::global_frontier::{
    GlobalFact, GlobalFrontier, GlobalSourceAuthority, GlobalStep, InlineReferenceResolution,
    LeafBlockKind, VerifiedGlobalReference,
};
use std::rc::Rc;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct MarkdownTableUnderlineRequestStep {
    /// Actual pairing/UTF-16 cursor work, separate from structural replay and repeated prefix decoding.
    pub source_bytes: usize,
    pub utf8_validation_bytes: usize,
    pub source_tree_steps: usize,
    /// Actual same-Global replay for code permission; the parent also charges it to cumulative fuel.
    pub global_source_bytes: usize,
    pub global_source_tree_steps: usize,
    pub structural_transitions: usize,
    pub copied_source_bytes: usize,
    pub metadata_bytes: usize,
    pub normalizer_input_bytes: usize,
    pub parser_input_bytes: usize,
    pub allocated_source_capacity: usize,
    pub ready: bool,
}

#[derive(Debug)]
enum Phase {
    BeforeHtml,
    Retain,
    Rewind,
    ReferenceSource,
    Classify,
    Reference,
    Inline,
    Read {
        end: usize,
        literal: Option<Range<usize>>,
        real_eof: bool,
    },
    Restore,
    Publish,
    Ready,
}

#[derive(Clone, Copy, Debug)]
/// Construction fixes the scanner's real next owner; an ordinary window finishes scanning on UTF-16 publication.
enum UnderlineContextUse {
    Window,
    StreamedSource,
}

#[derive(Debug)]
pub(crate) struct MarkdownTableUnderlineRequest {
    // This transaction owns one scanner, one current inline/reference operation
    // and one external stack file. Its source identity is weak; body bytes remain document-owned.
    source: MarkdownTableSourceIdentity,
    pub(in crate::markdown_document_buffer) stack: StackFile,
    quota: MarkdownTableWindowQuota,
    pub(super) context: Option<MarkdownTableUnderlineContext>,
    context_use: UnderlineContextUse,
    pairs: Vec<MarkdownTableUnderline>,
    phase: Phase,
    references: Option<Box<ReferenceResolution>>,
    inline: Option<Box<InlinePermission>>,
}

#[derive(Debug)]
/// Allocate only when actual references require it, rather than embedding this workspace in every plain/huge block.
struct ReferenceResolution {
    queries: Rc<MarkdownTableReferenceQueries>,
    fact: Option<VerifiedGlobalReference>,
    request: Option<table_reference_request::Request>,
}

impl MarkdownDocumentBuffer {
    /// Retain bounded coverage, original weak source identity and one scanner, never body text or historical code ranges.
    pub(crate) fn table_underline_request(
        &self,
        byte_coverage: &[Range<usize>],
        quota: MarkdownTableWindowQuota,
    ) -> Result<MarkdownTableUnderlineRequest, MarkdownTableWindowError> {
        let context =
            MarkdownTableUnderlineContext::new(self.generation(), self.byte_len(), byte_coverage)?;
        self.table_underline_from_context(context, quota, UnderlineContextUse::Window)
    }

    /// Use a cell sealed by the same actual scanner; a huge cell adds scan rounds, not a whole-cell String.
    pub(crate) fn table_cell_underline_request(
        &self,
        cell: &super::super::VerifiedMarkdownTableCellSource,
        quota: MarkdownTableWindowQuota,
    ) -> Result<MarkdownTableUnderlineRequest, super::super::MarkdownTableRequestError> {
        let seed = cell
            .inline_seed_for(self)?
            .expect("非空 CellInline 保同 scanner 的原 seed");
        self.table_source_underline_request(seed.physical_range(), quota)
            .map_err(super::super::MarkdownTableRequestError::Window)
    }

    /// Hand off one current SourceRequest range serially, preserving its scanner/UTF-16 frontier without window history.
    pub(crate) fn table_source_underline_request(
        &self,
        byte_coverage: Range<usize>,
        quota: MarkdownTableWindowQuota,
    ) -> Result<MarkdownTableUnderlineRequest, MarkdownTableWindowError> {
        let context = MarkdownTableUnderlineContext::for_streamed_source(
            self.generation(),
            self.byte_len(),
            byte_coverage,
        )?;
        self.table_underline_from_context(context, quota, UnderlineContextUse::StreamedSource)
    }

    /// Build the sole owner graph for a context whose actual range was established by its corresponding entry point.
    fn table_underline_from_context(
        &self,
        context: MarkdownTableUnderlineContext,
        quota: MarkdownTableWindowQuota,
        context_use: UnderlineContextUse,
    ) -> Result<MarkdownTableUnderlineRequest, MarkdownTableWindowError> {
        Ok(MarkdownTableUnderlineRequest {
            source: self
                .table_source_identity()
                .ok_or(MarkdownTableWindowError::InvalidSource)?,
            quota,
            context: Some(context),
            context_use,
            pairs: Vec::new(),
            phase: Phase::BeforeHtml,
            references: None,
            inline: None,
            stack: StackFile::new(),
        })
    }
}

impl MarkdownTableUnderlineRequest {
    /// Close the sole file only after the last actual inline consumer; Ready alone does not finish an outer CodeJob.
    /// The parent closes explicitly before publication, preserving close failure; retained scanners keep an empty file owner.
    pub(in crate::markdown_document_buffer) fn close_inline_stack(&mut self) -> Result<(), Error> {
        if self.inline.is_some() {
            return Err(Error::InvalidRequest);
        }
        self.stack.close()
    }

    /// Continue another cell on the same source, changing actual coverage/quota without resetting scanner/tag state.
    /// Backward coverage returns false so the caller retires this owner and obtains an ordinary cold context.
    pub(crate) fn continue_cell(
        &mut self,
        document: &MarkdownDocumentBuffer,
        cell: &super::super::VerifiedMarkdownTableCellSource,
        quota: MarkdownTableWindowQuota,
    ) -> Result<bool, Error> {
        document.original_backing_health().map_err(Error::Source)?;
        let seed = cell
            .inline_seed_for(document)?
            .expect("非空格持原 inline seed");
        self.continue_source(
            document,
            seed.physical_range(),
            cell.canonical_range_for(document)?.clone(),
            quota,
        )
    }

    /// Advance the same pairing owner serially over a range certified by the actual root leaf/cell.
    /// Reverse/overlapping ranges need a cold owner; unknown earlier tags cannot be treated as absent.
    pub(crate) fn continue_source(
        &mut self,
        document: &MarkdownDocumentBuffer,
        bytes: Range<usize>,
        utf16: Range<usize>,
        quota: MarkdownTableWindowQuota,
    ) -> Result<bool, Error> {
        document.original_backing_health().map_err(Error::Source)?;
        if !self.source.is_current(document)
            || !matches!(self.phase, Phase::Ready)
            || !matches!(self.context_use, UnderlineContextUse::StreamedSource)
        {
            return Ok(false);
        }
        let Some(context) = self.context.as_mut() else {
            return Ok(false);
        };
        if !context.continue_source(bytes, utf16, &mut self.pairs) {
            return Ok(false);
        }
        // A later source batch may supply a new joint quota, but actual retained
        // scanner/pair capacity still occupies the original bounded workspace.
        quota.reserve_underline_workspace().map_err(Error::Window)?;
        self.quota = quota;
        self.references = None; // Previous-cell labels are not new-cell queries; return their bounded strings with the owner.
        self.phase = if context.needs_classified_continuation() {
            Phase::Retain
        } else {
            Phase::BeforeHtml
        };
        Ok(true)
    }

    /// Resolve bounded labels by looking ahead with the same Global, preserving already-paired underline results.
    pub(crate) fn resolve_references(
        &mut self,
        queries: MarkdownTableReferenceQueries,
    ) -> Result<usize, Error> {
        if self.references.is_some() {
            return Err(Error::InvalidRequest);
        }
        let ready = queries.is_ready();
        let allocated = std::mem::size_of::<ReferenceResolution>()
            + std::mem::size_of::<MarkdownTableReferenceQueries>()
            + 2 * std::mem::size_of::<usize>(); // Include the Rc counters and sole inventory owner in actual allocation.
        self.quota
            .admit_metadata(allocated)
            .map_err(Error::Window)?;
        self.references = Some(Box::new(ReferenceResolution {
            queries: Rc::new(queries),
            fact: None,
            request: None,
        }));
        if !ready {
            if !matches!(self.phase, Phase::Ready | Phase::BeforeHtml) {
                return Err(Error::InvalidRequest);
            }
            if matches!(self.phase, Phase::Ready) {
                self.phase = Phase::Retain;
            } // The no-HTML path finishes real source scanning before retaining/rewinding this Global for references.
        }
        Ok(allocated)
    }

    /// Transfer queries only after restoring the original boundary; unresolved definitions cannot become local Missing.
    pub(crate) fn take_resolved_references(
        &mut self,
    ) -> Result<Option<MarkdownTableReferenceQueries>, Error> {
        if !matches!(self.phase, Phase::Ready) || self.references_pending() {
            return Err(Error::InvalidRequest);
        }
        // Restore has returned the original inline owner. This is now the last
        // actual claim on the same query inventory, not a cloned definition map.
        Ok(self.references.take().map(|state| {
            Rc::try_unwrap(state.queries).expect("原code解析器必须先于引用库存移交结束")
        }))
    }

    /// Look ahead only for genuinely unresolved labels; no transaction and a closed Missing answer are not Pending.
    fn references_pending(&self) -> bool {
        self.references
            .as_ref()
            .is_some_and(|state| !state.queries.is_ready())
    }

    /// Perform one source borrow or Global control operation per step, returning leases before publication state advances.
    pub(crate) fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        global: &mut GlobalFrontier,
        budget: usize,
    ) -> Result<MarkdownTableUnderlineRequestStep, Error> {
        if !self.source.is_current(document) {
            return Err(Error::SourceSuperseded);
        }
        if !(2048..=MAX_TABLE_SOURCE_FRAME_BYTES).contains(&budget) {
            // This internal entry admits a bounded control quantum. Actual parent
            // call sites additionally reserve their own control work before delegating.
            return Err(Error::InvalidRequest);
        }
        let mut work = MarkdownTableUnderlineRequestStep {
            structural_transitions: 64,
            ..Default::default()
        };
        let authority = GlobalSourceAuthority {
            current_revision: document.generation(),
            cancelled: false,
        };
        match &self.phase {
            Phase::Ready => {
                return Ok(MarkdownTableUnderlineRequestStep {
                    ready: true,
                    ..Default::default()
                });
            }
            Phase::Retain => {
                // Retain only an actual closed block or committed table-row
                // boundary. An illegal boundary cannot discard facts or spin as zero progress.
                let measured = global
                    .retain_semantic_boundary(authority, budget - 64)
                    .map_err(Error::Global)?
                    .ok_or(Error::Window(MarkdownTableWindowError::ContextPending))?;
                work.structural_transitions += measured.transitions;
                self.phase = Phase::Rewind;
            }
            Phase::Rewind => {
                // Rewind this same Global to its real root while preserving
                // cumulative work, original limits and the one-shot restore point.
                let measured = global
                    .rewind_semantic_source_to_start(authority, budget - 64)
                    .map_err(Error::Global)?;
                work.structural_transitions += measured.transitions;
                self.phase = if self.references_pending() {
                    Phase::ReferenceSource
                } else {
                    Phase::Classify
                };
            }
            Phase::Restore => {
                if self.inline.take().is_some() {
                    // Return the bounded Parser/input and current code cursor in
                    // their own step before restoring the original Global position.
                    work.structural_transitions += 2048;
                    return Ok(work);
                }
                // Every lookahead frame has already been returned. Paired output
                // stays unreadable until the original boundary is actually restored.
                let measured = global
                    .restore_semantic_boundary(authority, budget - 64)
                    .map_err(Error::Global)?;
                work.structural_transitions += measured.transitions;
                self.phase = Phase::Publish;
            }
            Phase::Publish => {
                // Transfer the scanner's actual UTF-16 pairs once; style consumers
                // read only the published vector, never intermediate byte candidates.
                if let Some(context) = self.context.as_mut() {
                    self.pairs = context
                        .take_ready_utf16_pairs()
                        .ok_or(Error::Window(MarkdownTableWindowError::ContextPending))?;
                } // Reference-only replay retains the same already-published bounded pairs.
                if matches!(self.context_use, UnderlineContextUse::Window) {
                    // Ordinary windows have no serial-cell frontier consumer.
                    // Return the completed scanner and test-only byte directory
                    // immediately, preserving their completion-time release contract.
                    self.context = None;
                }
                // Only the serial-source entry retains the actual scanner/UTF-16
                // frontier for its next window/cell. Its result vector moved out;
                // no body text, second Global or document-wide tag index remains.
                self.phase = Phase::Ready;
                work.ready = true;
            }
            Phase::Classify => {
                if self
                    .context
                    .as_ref()
                    .is_none_or(|context| context.is_ready())
                    && !self.references_pending()
                {
                    self.phase = Phase::Restore;
                    return Ok(work);
                }
                return self.advance_global(document, global, authority, budget);
            }
            Phase::ReferenceSource => {
                if !self.references_pending() {
                    // Close definitions in original Global order before replaying
                    // code/HTML precedence. Otherwise a code event awaiting its
                    // reference could also block the definition needed to resolve it.
                    self.phase = Phase::Rewind;
                    return Ok(work);
                }
                return self.advance_global(document, global, authority, budget);
            }
            Phase::Reference => return self.advance_reference(document, budget),
            Phase::Inline => return self.advance_inline(document, budget),
            Phase::BeforeHtml | Phase::Read { .. } => {
                return self.advance_context(document, budget);
            }
        }
        Ok(work)
    }

    /// Drive the original Global for one current permission; future facts do not enter the main source/history index.
    fn advance_global(
        &mut self,
        document: &MarkdownDocumentBuffer,
        global: &mut GlobalFrontier,
        authority: GlobalSourceAuthority,
        budget: usize,
    ) -> Result<MarkdownTableUnderlineRequestStep, Error> {
        let references_only = matches!(self.phase, Phase::ReferenceSource);
        let tree = document
            .root
            .as_ref()
            .map_or(0, |root| usize::from(root.height()));
        // Global's internal byte work is at most one quarter of its transition
        // allowance; this share also reserves actual source and AVL/control work.
        let child = budget.saturating_sub(tree + 64) * 4 / 5;
        if child < 2048 {
            return Err(Error::WorkLimit);
        }
        let frame = document
            .table_source_frame(global.next_byte(), child)
            .map_err(Error::Source)?;
        let step = global
            .advance(
                SourceFrame {
                    source_revision: frame.generation,
                    byte_start: frame.byte_start,
                    bytes: &frame.bytes,
                    real_eof: frame.real_eof,
                },
                authority,
                child,
            )
            .map_err(Error::Global);
        frame.finish().map_err(Error::Source)?; // Real reclaim failure outranks a candidate fact or restored-boundary claim.
        let mut metadata = 0;
        let (measured, permission) = match step? {
            GlobalStep::Progress { work, .. } | GlobalStep::NeedInput { work, .. } => (work, None),
            GlobalStep::Complete { work } => {
                if let Some(state) = self.references.as_mut()
                    && !state.queries.is_ready()
                {
                    Rc::get_mut(&mut state.queries)
                        .expect("解析期间尚未给inline借出引用库存")
                        .finish_verified_eof();
                }
                (work, Some((document.byte_len(), None, true)))
            }
            GlobalStep::Fact { fact, work } => {
                // Consume and return one fact at a time. Lookahead does not
                // overwrite the main structure's pending table/ordinary-source state.
                let (end, literal) = match fact {
                    GlobalFact::Table(header) => (header.separator_byte_end, None), // Global committed the actual header/delimiter.
                    GlobalFact::Row(row) => (row.byte_end, None), // Only actual member rows belong to this fact.
                    GlobalFact::TableEnd(end) => (end.byte_end(), None), // Reclassify a failed candidate in the following block.
                    GlobalFact::Footnote(_) => {
                        // Definition existence is not body-classification coverage.
                        // Keep the phase and return only this actual Global work.
                        return Ok(MarkdownTableUnderlineRequestStep {
                            global_source_bytes: work.byte_reads,
                            global_source_tree_steps: tree,
                            structural_transitions: work.transitions + tree + 64,
                            ..Default::default()
                        });
                    }
                    GlobalFact::Reference(reference) => {
                        let end = reference.candidate().span_end;
                        if self.references_pending() {
                            let state = self.references.as_mut().unwrap();
                            state.fact = Some(reference);
                            self.phase = Phase::Reference;
                        }
                        (end, None) // Do not adopt a title lookahead backup tail; transfer the actual fact once.
                    }
                    // A closed inline block grants source/scope, not inline-code
                    // precedence. Its actual code cursor must still classify that source.
                    GlobalFact::InlineBlock(block) => {
                        let end = block.physical_range().end;
                        if !references_only
                            && self.context.as_ref().is_some_and(|context| {
                                !context.is_ready() && context.next_byte() < end
                            })
                        {
                            metadata = std::mem::size_of::<InlinePermission>();
                            if metadata > budget {
                                return Err(Error::WorkLimit);
                            }
                            self.inline = Some(Box::new(InlinePermission::new(block)?));
                            self.phase = Phase::Inline;
                        }
                        (end, None) // The actual child cursor still proves code/HTML precedence under this same fact.
                    }
                    GlobalFact::LeafBlock(block) => {
                        let end = block.consumed_end();
                        if !references_only
                            && matches!(block.kind(), LeafBlockKind::AtxHeading(_))
                            && self.context.as_ref().is_some_and(|context| {
                                !context.is_ready() && context.next_byte() < end
                            })
                        {
                            let metadata = std::mem::size_of::<InlinePermission>();
                            if metadata > budget {
                                return Err(Error::WorkLimit);
                            }
                            self.inline = Some(Box::new(InlinePermission::from_atx(block)?));
                            self.phase = Phase::Inline;
                            return Ok(MarkdownTableUnderlineRequestStep {
                                global_source_bytes: work.byte_reads,
                                global_source_tree_steps: tree,
                                structural_transitions: work.transitions + tree + 64,
                                metadata_bytes: metadata,
                                ..Default::default()
                            }); // Feed real leaf code events first so literal heading HTML cannot alter outer pairing.
                        }
                        let literal = matches!(
                            block.kind(),
                            LeafBlockKind::FencedCode | LeafBlockKind::IndentedCode
                        )
                        .then(|| block.physical_start()..end);
                        (end, literal)
                    }
                };
                (work, Some((end, literal, false)))
            }
        };
        if let Some((end, literal, real_eof)) = permission
            && !matches!(self.phase, Phase::Reference | Phase::Inline)
        {
            if references_only {
                // The first pass consumes definition facts only; leave the sole
                // underline scanner's UTF-16 cursor and pending tags at their original position.
                self.phase = Phase::ReferenceSource;
            } else if self
                .context
                .as_ref()
                .is_none_or(|context| context.is_ready())
            {
                self.phase = Phase::Classify;
            } else {
                self.phase = Phase::Read {
                    end,
                    literal,
                    real_eof,
                };
            }
        }
        Ok(MarkdownTableUnderlineRequestStep {
            global_source_bytes: measured.byte_reads,
            global_source_tree_steps: tree,
            structural_transitions: measured.transitions + tree + 64,
            metadata_bytes: metadata,
            ..Default::default()
        })
    }

    /// Hand off literals from the same code owner, with surrounding plain text consumed by the sole scanner in order.
    fn advance_inline(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<MarkdownTableUnderlineRequestStep, Error> {
        if self.inline.as_ref().unwrap().reference_input()?.is_some() {
            if self.references.is_none() {
                // Plain targets have no reference projection. Create the sole
                // inventory only when this actual backtick-bearing block needs Parser.
                let mut queries =
                    MarkdownTableReferenceQueries::for_batch(Some(self.quota.clone()));
                queries.seal_batch();
                let allocated = self.resolve_references(queries)?;
                return Ok(MarkdownTableUnderlineRequestStep {
                    metadata_bytes: allocated,
                    structural_transitions: 64,
                    ..Default::default()
                }); // Separate owner allocation from subsequent bounded Parser collection work.
            }
            let inline = self.inline.as_mut().unwrap();
            let input = inline.reference_input()?.unwrap();
            let state = self.references.as_mut().unwrap();
            let queries =
                Rc::get_mut(&mut state.queries).expect("当前inline尚未向Parser借出原查询库存");
            let before = self.quota.metadata_used();
            let mut work = MarkdownTableUnderlineRequestStep {
                structural_transitions: 64,
                ..Default::default()
            };
            let streamed = match input {
                ReferenceInput::Bounded { source, options } => {
                    // This original bounded block supplies its actual options;
                    // classification queries share the one inventory, not a second parser of definitions.
                    work.parser_input_bytes = source.len();
                    queries
                        .collect_for_classification(source, options)
                        .map_err(Error::Window)?;
                    None
                }
                ReferenceInput::Streamed(label) => {
                    // Only a verified complete label can enter the finite
                    // normalizer. Missing requires the real closed inventory result.
                    work.normalizer_input_bytes = label.len();
                    queries
                        .collect_verified_label(label)
                        .map_err(Error::Window)?;
                    Some(if queries.is_ready() {
                        Some(
                            if queries
                                .resolve_for_classification(label)
                                .map_err(Error::Window)?
                                .is_some()
                            {
                                InlineReferenceResolution::Resolved
                            } else {
                                InlineReferenceResolution::Missing
                            },
                        )
                    } else {
                        None
                    })
                }
            };
            work.metadata_bytes = self.quota.metadata_used() - before;
            if state.queries.is_ready() {
                if let Some(Some(resolution)) = streamed {
                    let accepted = inline.accept_reference(
                        resolution,
                        GlobalSourceAuthority {
                            current_revision: document.generation(),
                            cancelled: false,
                        },
                    )?;
                    work.structural_transitions += accepted.transitions;
                } else {
                    inline.accept_references(Rc::clone(&state.queries));
                }
            } else {
                // No child classification result has escaped. Return that owner
                // and replay the same Global to this block after definitions resolve;
                // the original inventory and the outer CodeJob's paused records remain unique.
                self.inline = None;
                self.phase = Phase::Rewind;
                work.structural_transitions += 2048;
            }
            return Ok(work);
        }
        let inline = self.inline.as_mut().unwrap();
        let step = inline.advance(document, &mut self.stack, &self.quota, budget)?;
        if let Some(literal) = step.literal {
            self.phase = Phase::Read {
                end: literal.end,
                literal: Some(literal),
                real_eof: false,
            };
        } else if step.done {
            self.phase = Phase::Read {
                end: inline.end(),
                literal: None,
                real_eof: false,
            };
        }
        Ok(MarkdownTableUnderlineRequestStep {
            global_source_bytes: step.work.source_bytes,
            global_source_tree_steps: step.work.source_tree_steps,
            copied_source_bytes: step.work.copied_source_bytes,
            metadata_bytes: step.work.metadata_bytes,
            parser_input_bytes: step.work.parser_input_bytes,
            allocated_source_capacity: step.work.allocated_source_capacity,
            normalizer_input_bytes: step.work.normalizer_input_bytes,
            utf8_validation_bytes: step.work.utf8_validation_bytes,
            structural_transitions: step.work.structural_transitions,
            ..Default::default()
        })
    }

    /// Parse one actual definition through its existing scope/normalizer transaction, never substituting a later namesake.
    fn advance_reference(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<MarkdownTableUnderlineRequestStep, Error> {
        let state = self.references.as_mut().ok_or(Error::InvalidRequest)?;
        if let Some(fact) = state.fact.take() {
            // Consume this verified fact once into its actual definition request,
            // retaining neither an alternate seed nor a copied definition inventory.
            state.request =
                table_reference_request::Request::new(self.source.clone(), fact, &state.queries)
                    .map_err(Error::Window)?;
            return Ok(MarkdownTableUnderlineRequestStep {
                structural_transitions: 64,
                ..Default::default()
            });
        }
        if let Some(request) = state.request.as_mut() {
            let step = request
                .advance(
                    document,
                    Rc::get_mut(&mut state.queries)
                        .expect("引用首遍与inline解释不并行持有可变库存"),
                    budget,
                )
                .map_err(Error::Window)?;
            if step.complete {
                state.request = None;
            }
            return Ok(MarkdownTableUnderlineRequestStep {
                global_source_bytes: step.work.source_bytes,
                global_source_tree_steps: step.work.source_tree_steps,
                copied_source_bytes: step.work.copied_bytes,
                metadata_bytes: step.work.metadata_bytes,
                structural_transitions: step.work.structural_transitions,
                normalizer_input_bytes: step.work.normalizer_input_bytes,
                parser_input_bytes: step.work.parser_input_bytes,
                ..Default::default()
            });
        }
        // Definition work belongs only to the first pass. Its endpoint does not
        // authorize the underline scanner before actual code precedence is established.
        self.phase = Phase::ReferenceSource;
        Ok(MarkdownTableUnderlineRequestStep {
            structural_transitions: 64,
            ..Default::default()
        })
    }

    /// Interpret tags only in proven bytes; the no-HTML fast path stops before its first candidate with UTF-16 progress intact.
    fn advance_context(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<MarkdownTableUnderlineRequestStep, Error> {
        let context = self.context.as_mut().unwrap();
        if context.is_ready() {
            // Empty coverage also transfers once. For nonempty scans, the phase
            // records whether original Global restoration must precede publication.
            self.phase = if self.references_pending() {
                if matches!(self.phase, Phase::BeforeHtml) {
                    Phase::Retain
                } else {
                    Phase::Classify
                }
            } else {
                Phase::Publish
            };
            return Ok(MarkdownTableUnderlineRequestStep {
                structural_transitions: 64,
                ..Default::default()
            });
        }
        let next = context.next_byte();
        let tree = document
            .root
            .as_ref()
            .map_or(0, |root| usize::from(root.height()));
        let maximum = (budget.saturating_sub(tree + 64) / 8).clamp(1, 4096);
        let (end, literal, proven_eof) = match &self.phase {
            Phase::BeforeHtml => (document.byte_len(), false, false),
            Phase::Read {
                end,
                literal,
                real_eof,
            } => {
                if next >= *end && !real_eof {
                    // A fast-path prefix or adjacent fact may add no bytes. Return
                    // to the actual Global/code owner for further permission, not synthetic EOF.
                    self.phase = if self.inline.as_ref().is_some_and(|inline| !inline.done()) {
                        Phase::Inline
                    } else {
                        self.inline = None;
                        Phase::Classify
                    };
                    return Ok(MarkdownTableUnderlineRequestStep {
                        structural_transitions: 64,
                        ..Default::default()
                    });
                }
                let is_literal = literal.as_ref().is_some_and(|range| range.contains(&next));
                let end = literal
                    .as_ref()
                    .filter(|range| next < range.start)
                    .map_or(*end, |range| range.start);
                (end, is_literal, *real_eof)
            }
            _ => unreachable!("只有源读取阶段借用正文"),
        };
        let frame = document
            .table_source_frame(next, maximum.min(end.saturating_sub(next)).max(1))
            .map_err(Error::Source)?;
        let count = frame.bytes.len().min(end.saturating_sub(next));
        let count = if matches!(self.phase, Phase::BeforeHtml) {
            // Leave the first actual '<' outside the scanner until structural
            // classification can prove whether code/literal context suppresses it.
            frame.bytes[..count]
                .iter()
                .position(|byte| *byte == b'<')
                .unwrap_or(count)
        } else {
            count
        };
        let html = matches!(self.phase, Phase::BeforeHtml) && frame.bytes.get(count) == Some(&b'<');
        let real_eof = frame.real_eof
            && count == frame.bytes.len()
            && (matches!(self.phase, Phase::BeforeHtml) || proven_eof);
        let input = MarkdownTableContextFrame {
            // Source EOF and classification-proven EOF must agree. A shorter
            // permitted range or physical piece alone cannot close pending tags.
            generation: frame.generation,
            byte_start: frame.byte_start,
            bytes: &frame.bytes[..count],
            real_eof,
        };
        let step = if literal {
            context.advance_literal(input, maximum)
        } else {
            context.advance(input, maximum)
        };
        // Return the real source lease before interpreting candidate pairing work;
        // source reclaim failure must not become a successful published style.
        frame.finish().map_err(Error::Source)?;
        let step = step.map_err(Error::Window)?;
        if step.ready {
            self.phase = if self.references_pending() {
                if matches!(self.phase, Phase::BeforeHtml) {
                    Phase::Retain
                } else {
                    Phase::Classify
                }
            } else if matches!(self.phase, Phase::BeforeHtml) {
                Phase::Publish
            } else {
                Phase::Restore
            };
        } else if html {
            // The first potential HTML byte remains unconsumed. Retain its actual
            // UTF-16 position while acquiring original structural permission.
            self.phase = Phase::Retain;
        }
        Ok(MarkdownTableUnderlineRequestStep {
            source_bytes: step.consumed,
            utf8_validation_bytes: step.utf8_validation_bytes,
            source_tree_steps: tree,
            structural_transitions: step.consumed + step.utf8_validation_bytes + tree + 64,
            ..Default::default()
        })
    }

    /// Expose pairs only after lease return and original Global restoration; stale generations cannot reuse published results.
    pub(crate) fn ready_pairs_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<Option<&[MarkdownTableUnderline]>, MarkdownTableWindowError> {
        if !self.source.is_current(document) {
            return Err(MarkdownTableWindowError::InvalidSource);
        }
        Ok(matches!(self.phase, Phase::Ready).then_some(self.pairs.as_slice()))
    }
}
