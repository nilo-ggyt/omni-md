//! Derive code permission from the same Global's closed inline block, retaining one current fact/cursor/span.

use super::inline_reference::ReferenceLabel;
use super::inline_stack::StackFile;
use super::*;
use crate::markdown_document_buffer::table_window_request::{
    MarkdownTableRequestError as Error, SemanticWork,
};
use crate::markdown_viewport_presentation::table_window::MarkdownTableWindowQuota;
use crate::markdown_viewport_presentation::table_window::references::MarkdownTableReferenceQueries;
use pulldown_cmark::block_frontier::SourceFrame;
use pulldown_cmark::global_frontier::{
    GlobalSourceAuthority, GlobalWork, InlineCodeCursor, InlineCodeStep, InlineLinkStorageRequest,
    InlineLinkStorageStep, InlinePlainLimits, InlineReferenceResolution, LeafBlockKind,
    VerifiedGlobalInlineBlock, VerifiedGlobalLeafBlock,
};
use pulldown_cmark::{BrokenLink, CowStr, Event, OffsetIter, Options, Parser};
use std::{cell::Cell, ops::Range, rc::Rc};

type Callback = Box<dyn FnMut(BrokenLink<'static>) -> Option<(CowStr<'static>, CowStr<'static>)>>;

/// Drop Parser before its fixed input; this private owner exposes no text replacement or escaping borrow.
struct SmallParser {
    events: OffsetIter<'static, Callback>,
    _text: Box<str>,
    _references: Rc<MarkdownTableReferenceQueries>,
    failure: Rc<Cell<Option<MarkdownTableWindowError>>>,
}
impl SmallParser {
    /// Give Parser a stable, exclusive allocation for its bounded input, at most 2 KiB.
    fn new(
        bytes: Vec<u8>,
        options: Options,
        references: Rc<MarkdownTableReferenceQueries>,
    ) -> Result<Self, Error> {
        let text = String::from_utf8(bytes)
            .map_err(|_| Error::InvalidRequest)?
            .into_boxed_str();
        // SAFETY: The unique text Box remains immutable inside this private owner;
        // moving Self cannot move its heap allocation and events drops before text.
        // No borrow escapes. The parent checks original source/reclaim authority,
        // and error/cancellation drops this entire owner.
        let source: &'static str = unsafe { &*(text.as_ref() as *const str) };
        let failure = Rc::new(Cell::new(None));
        let observed = Rc::clone(&failure);
        let callback_references = Rc::clone(&references);
        let callback: Callback = Box::new(move |link| {
            // Read the same Global's resolved inventory. Missing query authority
            // records its first error instead of guessing Missing or copying the map.
            let result = callback_references.resolve_for_classification(&link.reference);
            match result {
                Ok(value) => value.map(|(destination, title)| {
                    // SAFETY: Sealed query entries are immutable and the original
                    // Rc keeps their destination/title allocations alive throughout
                    // Parser use. events (including this callback) drops before
                    // _references. Event/Cow borrows are consumed only on this
                    // private owner's method stack and never stored or returned outward.
                    let destination: &'static str = unsafe { &*(destination as *const str) };
                    // SAFETY: The same immutable queries and original Rc retain
                    // title; this reference does not point into a temporary String.
                    let title: &'static str = unsafe { &*(title as *const str) };
                    (CowStr::from(destination), CowStr::from(title))
                }),
                Err(error) => {
                    // A callback cannot return the typed query failure directly.
                    // Preserve its first cause for the next event-consumer boundary.
                    if observed.get().is_none() {
                        observed.set(Some(error));
                    }
                    None
                }
            }
        });
        let events = Parser::new_with_broken_link_callback(source, options, Some(callback))
            .into_offset_iter();
        Ok(Self {
            events,
            _text: text,
            _references: references,
            failure,
        })
    }
}

enum Phase {
    VerifyRoot,
    Inspect,
    Copy,
    Queries,
    Parse,
    Events,
    Cursor,
    Done,
}

pub(super) struct InlinePermission {
    range: Range<usize>,
    next: usize,
    options: Options,
    root: bool,
    phase: Phase,
    bytes: Vec<u8>,
    parser: Option<SmallParser>,
    // Hold the original fact only until constructing its actual cursor; consume
    // it on that step rather than retaining another seed or parsed-result cache.
    block: Option<VerifiedGlobalInlineBlock>,
    cursor: Option<Box<InlineCodeCursor>>,
    pending_storage: Option<InlineLinkStorageRequest>,
    reference: Option<Box<ReferenceLabel>>,
    references: Option<Rc<MarkdownTableReferenceQueries>>,
}

pub(super) enum ReferenceInput<'a> {
    Bounded { source: &'a str, options: Options },
    Streamed(&'a str),
}

pub(super) struct PermissionStep {
    pub work: SemanticWork,
    pub literal: Option<Range<usize>>,
    pub done: bool,
}

/// Reserve this owner and the bounded 2-KiB input, reporting Parser input separately from AST internals.
/// Admit an optional large cursor on its actual construction quantum, without charging its payload twice.
pub(super) fn workspace_bytes() -> usize {
    2048 + std::mem::size_of::<InlinePermission>()
}

impl std::fmt::Debug for InlinePermission {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InlinePermission")
            .field("range", &self.range)
            .field("next", &self.next)
            .finish_non_exhaustive()
    }
}

impl InlinePermission {
    /// Inspect a real ATX leaf for backticks first; only an actual bounded root heading may use Parser proof.
    pub(super) fn from_atx(block: VerifiedGlobalLeafBlock) -> Result<Self, Error> {
        if !matches!(block.kind(), LeafBlockKind::AtxHeading(_)) {
            // A leaf from another grammar class cannot borrow the ATX-specific
            // shortcut merely because its source bytes resemble a heading.
            return Err(Error::InvalidRequest);
        }
        let range = block.physical_start()..block.source_range().end;
        Ok(Self {
            next: range.start,
            root: block.scope_depth() == 0,
            range,
            options: block.options(),
            phase: Phase::Inspect,
            bytes: Vec::new(),
            parser: None,
            block: None,
            cursor: None,
            pending_storage: None,
            reference: None,
            references: None,
        })
    }

    /// Derive root/scope/options/range from the actual fact, never caller booleans, bare ranges or lookahead.
    pub(super) fn new(block: VerifiedGlobalInlineBlock) -> Result<Self, Error> {
        let range = block.physical_range();
        let root = block.scope_depth() == 1;
        let options = block.options();
        Ok(Self {
            next: range.start,
            range,
            options,
            root,
            phase: Phase::Inspect,
            bytes: Vec::new(),
            parser: None,
            block: root.then_some(block),
            cursor: None,
            pending_storage: None,
            reference: None,
            references: None,
        })
    }

    pub(super) fn end(&self) -> usize {
        self.range.end
    }
    pub(super) fn done(&self) -> bool {
        matches!(self.phase, Phase::Done)
    }

    /// Collect references only for a genuinely copied bounded root block before Parser publishes code/HTML effects.
    pub(super) fn reference_input(&self) -> Result<Option<ReferenceInput<'_>>, Error> {
        if let Some(reference) = self.reference.as_ref() {
            return Ok(reference.label().map(ReferenceInput::Streamed));
        }
        if !matches!(self.phase, Phase::Queries) {
            return Ok(None);
        }
        let source = std::str::from_utf8(&self.bytes).map_err(|_| Error::InvalidRequest)?;
        Ok(Some(ReferenceInput::Bounded {
            source,
            options: self.options,
        }))
    }

    /// Resume the same pending reference only with the sole inventory's result; local absence is not Missing.
    pub(super) fn accept_reference(
        &mut self,
        resolution: InlineReferenceResolution,
        authority: GlobalSourceAuthority,
    ) -> Result<GlobalWork, Error> {
        let request = &self
            .reference
            .as_ref()
            .ok_or(Error::InvalidRequest)?
            .request;
        let work = self
            .cursor
            .as_mut()
            .ok_or(Error::InvalidRequest)?
            .accept_reference(request, resolution, authority)
            .map_err(Error::Global)?;
        self.reference = None;
        Ok(work)
    }

    /// Lend the sole completed query inventory to Parser without copying its map or parsing definitions again.
    pub(super) fn accept_references(&mut self, references: Rc<MarkdownTableReferenceQueries>) {
        self.references = Some(references);
        self.phase = Phase::Parse;
    }

    /// The original scanner skips a literal only after final code-span proof; finish every actual frame first.
    pub(super) fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        stack: &mut StackFile,
        quota: &MarkdownTableWindowQuota,
        budget: usize,
    ) -> Result<PermissionStep, Error> {
        document.original_backing_health().map_err(Error::Source)?;
        if self.block.is_some() {
            // The enclosing request supplies its admitted quantum. Construct and
            // charge the actual optional cursor separately from this small owner.
            let metadata = std::mem::size_of::<InlineCodeCursor>();
            if metadata > budget - 64 {
                return Err(Error::WorkLimit);
            }
            // Consume the verified fact into one exclusive cursor. Small owner
            // and large cursor have separate real allocations/lifetimes; retain
            // no historical fact after this ownership transfer.
            let block = self.block.take().unwrap();
            let end = if self.range.len() > 2048 {
                self.range.end
            } else {
                block.source_range().start
            };
            let cursor = block
                .into_code_cursor(
                    end,
                    InlinePlainLimits {
                        max_byte_work: usize::MAX,
                        max_transitions: usize::MAX,
                    },
                )
                .map_err(Error::Global)?;
            let metadata = if let Some(cursor) = cursor {
                quota.admit_metadata(metadata).map_err(Error::Window)?;
                self.cursor = Some(Box::new(cursor));
                self.phase = if self.range.len() > 2048 {
                    Phase::Cursor
                } else {
                    Phase::VerifyRoot
                };
                metadata
            } else {
                0
            };
            return Ok(PermissionStep {
                work: SemanticWork {
                    metadata_bytes: metadata,
                    structural_transitions: 64,
                    ..Default::default()
                },
                literal: None,
                done: false,
            });
        }
        if let Some(request) = self.pending_storage.as_ref() {
            // Complete the same vendor cursor's external record operation before
            // advancing its source again; only Ready clears the storage request.
            let result = stack.storage(request, quota, budget - 64);
            document.original_backing_health().map_err(Error::Source)?;
            let (step, metadata) = result?;
            let completed = matches!(&step, InlineLinkStorageStep::Ready { .. });
            let work = self
                .cursor
                .as_mut()
                .ok_or(Error::InvalidRequest)?
                .accept_storage(
                    request,
                    step,
                    GlobalSourceAuthority {
                        current_revision: document.generation(),
                        cancelled: false,
                    },
                    budget - 64,
                )
                .map_err(Error::Global)?;
            if completed {
                self.pending_storage = None;
            }
            return Ok(PermissionStep {
                work: SemanticWork {
                    structural_transitions: work.transitions + 64,
                    metadata_bytes: metadata,
                    ..Default::default()
                },
                literal: None,
                done: false,
            });
        }
        if let Some(reference) = self.reference.as_mut() {
            // The original cursor remains suspended while its one bounded label
            // decodes; an unresolved reference cannot be silently classified away.
            return reference
                .advance(document, quota, budget)
                .map(|work| PermissionStep {
                    work,
                    literal: None,
                    done: false,
                });
        }

        let mut work = SemanticWork {
            structural_transitions: 64,
            ..Default::default()
        };
        match self.phase {
            Phase::Queries => unreachable!("有限code块必须先由原配对owner完成引用库存交接"),
            Phase::Done => {
                return Ok(PermissionStep {
                    work,
                    literal: None,
                    done: true,
                });
            }
            Phase::Parse => {
                // Move the bounded input into the stable private parser owner.
                // The reference inventory was completed by the parent transaction.
                work.parser_input_bytes = self.bytes.len();
                self.parser = Some(SmallParser::new(
                    std::mem::take(&mut self.bytes),
                    self.options,
                    self.references
                        .take()
                        .expect("父事务已交付当前块的唯一已闭合引用库存"),
                )?);
                self.phase = Phase::Events;
            }
            Phase::Events => {
                let parser = self.parser.as_mut().unwrap();
                // Consume one actual event per step. The prior quantum admitted
                // fixed input; do not accumulate an entire block's event-range directory.
                let event = parser.events.next();
                if let Some(error) = parser.failure.get() {
                    return Err(Error::Window(error));
                }
                match event {
                    Some((Event::Code(_), range)) => {
                        // Translate only the actual Parser code event into the
                        // original source range used by the unique underline scanner.
                        return Ok(PermissionStep {
                            work,
                            literal: Some(
                                self.range.start + range.start..self.range.start + range.end,
                            ),
                            done: false,
                        });
                    }
                    Some(_) => {}
                    None => {
                        // No events remain. Release Parser and its stable input
                        // before declaring this permission owner finished.
                        self.parser = None;
                        self.phase = Phase::Done;
                    }
                }
            }
            Phase::Cursor | Phase::VerifyRoot => {
                // A verified cursor proves root/code semantics over original
                // pieces, reserving tree/control work before its child quantum.
                let verifying = matches!(self.phase, Phase::VerifyRoot);
                let tree = document
                    .root
                    .as_ref()
                    .map_or(0, |root| usize::from(root.height()));
                let child = budget.saturating_sub(tree + 64) * 4 / 5;
                if child < 2048 {
                    return Err(Error::WorkLimit);
                }
                let cursor = self.cursor.as_mut().unwrap();
                let frame = document
                    .table_source_frame(cursor.next_byte(), child)
                    .map_err(Error::Source)?;
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
                    .map_err(Error::Global);
                // Reclaim the actual input lease before inspecting cursor facts;
                // a source failure must outrank a candidate code span.
                frame.finish().map_err(Error::Source)?;
                let step = step?;
                let allocated = cursor.allocated_workspace_bytes() - workspace_before;
                quota.admit_metadata(allocated).map_err(Error::Window)?;
                work.metadata_bytes += allocated;
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
                // Storage/reference may suspend before consuming the whole frame;
                // source accounting still records the actual requested leased slice.
                work.source_bytes = source_bytes;
                work.structural_transitions += measured.transitions + tree;
                work.source_tree_steps = tree;
                match step {
                    InlineCodeStep::Storage { request, .. } => self.pending_storage = Some(request),
                    InlineCodeStep::Reference { request, .. } => {
                        // Allocate only this verified label owner; its decoder
                        // and returned inventory result belong to the same paused cursor.
                        let metadata = std::mem::size_of::<ReferenceLabel>();
                        quota.admit_metadata(metadata).map_err(Error::Window)?;
                        work.metadata_bytes += metadata;
                        self.reference = Some(Box::new(ReferenceLabel::new(request)));
                    }
                    InlineCodeStep::Span { proof, .. } => {
                        return Ok(PermissionStep {
                            work,
                            literal: Some(proof.source_range()),
                            done: false,
                        });
                    }
                    InlineCodeStep::Proven { .. } => {
                        // Return the finished cursor. Root verification resumes
                        // inspection; a fully streamed proof finishes this owner.
                        self.cursor = None;
                        self.phase = if verifying {
                            Phase::Inspect
                        } else {
                            Phase::Done
                        };
                    }
                    InlineCodeStep::NeedsGeneralInline { .. } => {
                        self.cursor = None;
                        self.next = self.range.start;
                        self.phase = Phase::Inspect; // A no-backtick block can still be proved; an actual tick stays unknown.
                    }
                    _ => {}
                }
            }
            Phase::Inspect | Phase::Copy => {
                // Inspect first without retaining body text. Only an actual tick
                // in an eligible bounded root block permits the explicit bounded copy.
                if self.next == self.range.end {
                    self.phase = if matches!(self.phase, Phase::Inspect) {
                        Phase::Done
                    } else {
                        Phase::Queries
                    };
                    return Ok(PermissionStep {
                        work,
                        literal: None,
                        done: self.done(),
                    });
                }
                let tree = document
                    .root
                    .as_ref()
                    .map_or(0, |root| usize::from(root.height()));
                let maximum = (budget.saturating_sub(tree + 64) / 4).max(1);
                let frame = document
                    .table_source_frame(self.next, maximum.min(self.range.end - self.next))
                    .map_err(Error::Source)?;
                let count = frame.bytes.len().min(self.range.end - self.next);
                let tick =
                    matches!(self.phase, Phase::Inspect) && frame.bytes[..count].contains(&b'`');
                if matches!(self.phase, Phase::Copy) {
                    self.bytes.extend_from_slice(&frame.bytes[..count]);
                    work.copied_source_bytes = count;
                }
                self.next += count;
                frame.finish().map_err(Error::Source)?;
                work.source_bytes = count;
                work.source_tree_steps = tree;
                work.structural_transitions += count + tree;
                if tick {
                    // Larger or scoped blocks require real general-inline
                    // permission; never flatten them to obtain a speculative result.
                    if !self.root || self.range.len() > 2048 {
                        return Err(Error::Window(MarkdownTableWindowError::ContextPending));
                    }
                    self.bytes = Vec::with_capacity(self.range.len());
                    work.allocated_source_capacity = self.bytes.capacity();
                    self.next = self.range.start;
                    self.phase = Phase::Copy;
                }
            }
        }
        Ok(PermissionStep {
            work,
            literal: None,
            done: self.done(),
        })
    }
}
