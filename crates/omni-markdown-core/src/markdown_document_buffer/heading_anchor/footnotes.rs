//! Bounded footnote queries for the current heading. Each real key independently
//! scans the original global frontier without retaining all definitions or their text.
//! Certified definition keys compare through streaming case folding, so even
//! extremely long unrelated keys are never materialized as complete strings.

use super::*;
use pulldown_cmark::global_frontier::{InlineReferenceDecoder, VerifiedGlobalFootnoteDefinition};
use unicase::UniCase;

// Labels are normalized identifiers belonging to this heading only. `None`
// means unknown, never a missing definition proved by the whole source.
// Source definitions do not enter this inventory. Duplicate keys reuse the same
// actual answer after comparison under the existing UniCase rules.
struct Query {
    label: String,
    found: Option<bool>,
}

// A current definition retains its shared decoder continuation and comparison
// offset only; the real global fact already certified the complete source syntax.
// Later text cannot repair a mismatching prefix. Drop that decoder immediately
// and resume the same global frontier without reading the rest of this key.
struct Definition {
    decoder: InlineReferenceDecoder,
    end: usize,
    compared: usize,
}

pub(super) struct HeadingFootnotes {
    queries: Vec<Query>,
    query_bytes: usize,
    current: usize,
    folded: String,
    global: Option<GlobalFrontier>,
    definition: Option<Definition>,
    quota: MarkdownTableWindowQuota,
    failure: Option<Error>,
}

impl HeadingFootnotes {
    /// Stores only this heading's parser/cursor-certified keys, with no strong
    /// references to the document, source text, or history.
    pub(super) fn new(quota: MarkdownTableWindowQuota) -> Self {
        Self {
            queries: Vec::new(),
            query_bytes: 0,
            current: 0,
            folded: String::new(),
            global: None,
            definition: None,
            quota,
            failure: None,
        }
    }

    /// The lookup callback can report unknown only. Preserve actual admission failure
    /// as its original typed cause for the owner to return immediately after parsing.
    pub(super) fn lookup(&mut self, label: &str) -> Option<bool> {
        if let Some(query) = self
            .queries
            .iter()
            .find(|query| UniCase::new(query.label.as_str()) == UniCase::new(label))
        {
            return query.found;
        }
        if self.failure.is_some() {
            // A prior admission failure revokes further query growth until the owner
            // takes that exact error; unknown remains distinct from a missing result.
            return None;
        }
        let bytes = self.query_bytes.checked_add(label.len());
        if bytes.is_none_or(|bytes| bytes > FRAGMENT_LIMIT) {
            // Reject before copying this key or allocating another handle page.
            // The parser's optional-answer channel cannot conceal this typed failure.
            self.failure = Some(Error::Semantic(
                super::super::table_window_request::MarkdownTableRequestError::Window(
                    crate::markdown_viewport_presentation::table_window::MarkdownTableWindowError::ReferenceLimit,
                ),
            ));
            return None;
        }
        // Grow the handle directory only when full. Move existing owned strings
        // into its new capacity rather than copying their identifier bytes.
        let capacity = if self.queries.len() == self.queries.capacity() {
            self.queries.capacity().saturating_mul(2).max(4)
        } else {
            0
        };
        let additional = label.len() + std::mem::size_of::<Query>() * capacity;
        if let Err(error) = self.quota.admit_metadata(additional) {
            self.failure = Some(Error::Semantic(
                super::super::table_window_request::MarkdownTableRequestError::Window(error),
            ));
            return None;
        }
        if capacity != 0 {
            // Account for allocator rounding before transferring existing query owners;
            // failure drops only the new directory, retaining original query state.
            let mut next = Vec::with_capacity(capacity);
            if let Err(error) = self
                .quota
                .admit_metadata((next.capacity() - capacity) * std::mem::size_of::<Query>())
            {
                self.failure = Some(Error::Semantic(
                    super::super::table_window_request::MarkdownTableRequestError::Window(error),
                ));
                return None;
            }
            next.append(&mut self.queries);
            self.queries = next;
        }
        self.query_bytes = bytes.unwrap();
        self.queries.push(Query {
            label: label.to_owned(),
            found: None,
        });
        None
    }

    /// Every current query has either a found result or a missing result proved by
    /// real EOF. An empty query set creates no global scan or anonymous resources.
    pub(super) fn is_ready(&self) -> bool {
        self.current == self.queries.len()
    }
    /// Transfers the first actual lookup/admission failure to the original request
    /// owner instead of reclassifying the callback's unknown value as a missing key.
    pub(super) fn take_failure(&mut self) -> Option<Error> {
        self.failure.take()
    }

    /// A key completes only on a real match or global EOF. Lookahead facts cannot
    /// raise the semantic frontier of another source.
    pub(super) fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<(MarkdownHeadingAnchorWork, bool), Error> {
        if self.is_ready() {
            // An answered or empty query set needs no source read or new frontier.
            return Ok((MarkdownHeadingAnchorWork::default(), true));
        }
        if self.definition.is_some() {
            // Resume its exact bounded label decoder before asking for another fact.
            return self.compare_definition(document, budget);
        }
        // One current key owns one original global scan. Serial rescanning has
        // explicit CPU/I/O cost in exchange for bounded memory and truthful EOF.
        // Both success and failure release this request's resources; no cross-document
        // cache, background work, or whole-source footnote directory remains.
        if self.global.is_none() {
            // UniCase's full case fold is context-free. Materialize only the bounded
            // current query, never every source definition.
            self.quota
                .admit_metadata(self.queries[self.current].label.len().saturating_mul(4))
                .map_err(|error| {
                    Error::Semantic(
                        super::super::table_window_request::MarkdownTableRequestError::Window(
                            error,
                        ),
                    )
                })?;
            self.folded = UniCase::new(self.queries[self.current].label.as_str()).to_folded_case();
            let fuel = document
                .byte_len()
                .checked_mul(4096)
                .and_then(|n| n.checked_add(1024 * 1024))
                .ok_or(Error::ArithmeticOverflow)?;
            self.global = Some(
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
            );
            return Ok((
                MarkdownHeadingAnchorWork {
                    metadata_bytes: self.folded.capacity(),
                    structural_transitions: 128,
                    ..Default::default()
                },
                false,
            ));
        }
        let global = self.global.as_mut().unwrap();
        let frame = document.table_source_frame(global.next_byte(), PAGE)?;
        let source_bytes = frame.bytes.len();
        let step = global.advance(
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
        );
        frame.finish()?;
        // Only a real global footnote-definition fact can establish existence.
        // Body text, code, HTML, or similar raw characters cannot fabricate a match.
        // The source lease has finished; any actual read/reclamation failure
        // prevents entry into the successful fact-consumption path below.
        let (measured, fact) = match step.map_err(Error::Global)? {
            GlobalStep::Progress { work, .. } | GlobalStep::NeedInput { work, .. } => (work, None),
            GlobalStep::Fact { fact, work } => (work, Some(fact)),
            GlobalStep::Complete { work } => {
                self.complete_query(false); // Only whole-source EOF proves missing; absence from the viewport proves nothing.
                return Ok((
                    MarkdownHeadingAnchorWork {
                        source_bytes,
                        structural_transitions: work.transitions + 128,
                        ..Default::default()
                    },
                    false,
                ));
            }
        };
        if let Some(GlobalFact::Footnote(fact)) = fact {
            self.definition = Some(Self::definition(fact));
        }
        Ok((
            MarkdownHeadingAnchorWork {
                source_bytes,
                structural_transitions: measured.transitions + 128,
                ..Default::default()
            },
            false,
        ))
    }

    /// Retains the decoder and endpoint certified by this actual footnote fact,
    /// not the definition's text or a guessed range found by string searching.
    fn definition(fact: VerifiedGlobalFootnoteDefinition) -> Definition {
        Definition {
            decoder: fact.reference_decoder(),
            end: fact.reference_range().end,
            compared: 0,
        }
    }

    /// The shared label scanner emits complete bounded UTF-8 chunks for UniCase
    /// comparison. A mismatch can discard this already certified complete definition.
    /// Later characters cannot repair a different folded prefix, so no remaining
    /// definition-key bytes need to be read or retained.
    fn compare_definition(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<(MarkdownHeadingAnchorWork, bool), Error> {
        let definition = self.definition.as_mut().unwrap();
        let next = definition.decoder.next_byte();
        let frame = document.table_source_frame(next, (definition.end - next).min(256))?;
        let source_bytes = frame.bytes.len();
        let mut output = [0; 1024];
        let step = definition.decoder.advance(
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
            &mut output,
            budget - 128,
        );
        frame.finish()?; // Return the current lease before propagating decoder errors or advancing file/query work.
        let step = step.map_err(Error::Global)?;
        let text =
            std::str::from_utf8(&output[..step.written]).map_err(|_| Error::UnsupportedInline)?;
        self.quota
            .admit_metadata(text.len().saturating_mul(4))
            .map_err(|error| {
                Error::Semantic(
                    super::super::table_window_request::MarkdownTableRequestError::Window(error),
                )
            })?;
        let folded = UniCase::new(text).to_folded_case();
        let end = definition
            .compared
            .checked_add(folded.len())
            .ok_or(Error::ArithmeticOverflow)?;
        // Case folding does not reorder across scalars. Compare this fixed output
        // chunk with the corresponding range of the bounded query.
        // A found result additionally requires the decoder's actual closing bracket
        // and exactly equal complete folded lengths.
        let same = self.folded.as_bytes().get(definition.compared..end) == Some(folded.as_bytes());
        definition.compared = end;
        let exact = same && step.complete && end == self.folded.len();
        let work = MarkdownHeadingAnchorWork {
            source_bytes,
            copied_bytes: step.written + folded.len(),
            metadata_bytes: folded.capacity(),
            structural_transitions: step.work.transitions + folded.len() + 128,
            ..Default::default()
        };
        if exact {
            // A complete equal normalized key proves this query's existence; the
            // global scanner and current definition can release their structural pages.
            self.complete_query(true);
        } else if !same || step.complete {
            // A mismatching or unequal-length completed key answers nothing about
            // later definitions. Resume the same original global scan, keeping Unknown.
            self.definition = None;
        }
        Ok((work, false))
    }

    /// The query and global scan have one current owner. Delivering the conclusion
    /// releases its definition decoder and structural pages immediately.
    fn complete_query(&mut self, found: bool) {
        self.queries[self.current].found = Some(found);
        self.current += 1;
        self.definition = None;
        self.global = None;
        self.folded = String::new();
    }
}
