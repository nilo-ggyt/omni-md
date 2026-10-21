//! Resolves actual references for one current heading. Bounded queries contain
//! only its labels, never a whole-document definition map.

use super::*;
use crate::markdown_document_buffer::table_reference_request;
use crate::markdown_document_buffer::table_window_request::MarkdownTableRequestError;
use pulldown_cmark::CowStr;

pub(super) struct HeadingReferences {
    queries: MarkdownTableReferenceQueries,
    global: Option<GlobalFrontier>,
    pending: Option<table_reference_request::Request>,
}

impl HeadingReferences {
    /// Collects bounded actual heading references, then resolves them in the same
    /// source's global first-definition-wins order, including later definitions.
    pub(super) fn new(
        document: &MarkdownDocumentBuffer,
        queries: MarkdownTableReferenceQueries,
    ) -> Result<Self, Error> {
        let fuel = document
            .byte_len()
            .checked_mul(4096)
            .and_then(|n| n.checked_add(1024 * 1024))
            .ok_or(Error::ArithmeticOverflow)?;
        let global = if queries.is_ready() {
            // No unresolved heading label needs a whole-source structural scan.
            None
        } else {
            // Capture this document's actual generation/extent. Later definitions
            // remain visible, but no changed source can inherit this cursor's authority.
            Some(
                GlobalFrontier::new(
                    document.generation(),
                    document.byte_len(),
                    crate::markdown_viewport_presentation::parser::markdown_presentation_options(
                        true,
                    ),
                    GlobalFrontierLimits {
                        spine: StructuralSpineLimits {
                            max_depth: 16_384,
                            max_live_page_bytes: 4 * 1024 * 1024,
                            max_page_allocations: 1_000_000,
                            max_role_work: fuel,
                        },
                        max_byte_work: fuel,
                        max_transitions: fuel,
                    },
                )
                .map_err(Error::Global)?,
            )
        };
        Ok(Self {
            queries,
            global,
            pending: None,
        })
    }

    /// Parser callbacks borrow these bounded results only. Returning borrowed values
    /// creates no second URL/title buffer.
    pub(super) fn resolve<'a>(&'a self, label: &str) -> Option<(CowStr<'a>, CowStr<'a>)> {
        self.queries
            .resolve(label)
            .map(|(destination, title)| (destination.into(), title.into()))
    }

    /// Processes real definitions through the existing bounded normalizer/parser.
    /// Source reads, copies, and semantic work return to the original heading request.
    pub(super) fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<(MarkdownHeadingAnchorWork, bool), Error> {
        let mut work = MarkdownHeadingAnchorWork {
            structural_transitions: 1,
            ..Default::default()
        };
        if let Some(pending) = self.pending.as_mut() {
            // Complete the existing certified definition before advancing the global
            // frontier; its bounded decoder owns its own pending normalization.
            let step = pending
                .advance(document, &mut self.queries, budget)
                .map_err(|error| Error::Semantic(MarkdownTableRequestError::Window(error)))?;
            work.source_bytes = step.work.source_bytes;
            work.copied_bytes = step.work.copied_bytes;
            work.parser_input_bytes = step.work.parser_input_bytes;
            work.structural_transitions += step.work.structural_transitions;
            if step.complete {
                self.pending = None;
            }
            return Ok((work, false));
        }
        if self.queries.is_ready() {
            self.global = None; // First matching definitions answered every label; unrelated trailing text need not be scanned.
            return Ok((work, true));
        }
        let global = self.global.as_mut().ok_or(Error::ArithmeticOverflow)?;
        let frame = document.table_source_frame(global.next_byte(), PAGE)?;
        let borrowed = frame.bytes.len();
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
                budget - 128,
            )
            .map_err(Error::Global);
        // Return the actual source lease before inspecting the parser result. Real
        // reclamation failure takes precedence over a plausible definition/EOF fact.
        frame.finish()?;
        let (measured, fact) = match step? {
            GlobalStep::Progress { work, .. } | GlobalStep::NeedInput { work, .. } => (work, None),
            GlobalStep::Fact { fact, work } => (work, Some(fact)),
            GlobalStep::Complete { work } => {
                self.queries.finish_verified_eof(); // Only the original global frontier's real EOF authorizes a missing result.
                self.global = None;
                return Ok((
                    MarkdownHeadingAnchorWork {
                        source_bytes: borrowed,
                        structural_transitions: work.transitions + 1,
                        ..Default::default()
                    },
                    true,
                ));
            }
        };
        work.source_bytes = borrowed;
        work.structural_transitions += measured.transitions;
        if let Some(GlobalFact::Reference(fact)) = fact {
            // Only a verified global definition can start bounded value parsing.
            // Similar text in code, HTML, or ordinary paragraphs grants no lookup result.
            let source = document
                .table_source_identity()
                .ok_or(Error::ArithmeticOverflow)?;
            self.pending = table_reference_request::Request::new(source, fact, &self.queries)
                .map_err(|error| Error::Semantic(MarkdownTableRequestError::Window(error)))?;
        }
        Ok((work, false))
    }
}
