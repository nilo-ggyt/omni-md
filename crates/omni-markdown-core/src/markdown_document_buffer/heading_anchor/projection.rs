//! Inline projection of one actual giant heading. Retains original syntax
//! cursors, fixed pages, and an anonymous two-bit mask, never the complete heading.
//! Code, emphasis, and link facts close before visible characters decode in source
//! order, so a later delimiter cannot change a prefix already emitted.

use super::*;
use crate::markdown_document_buffer::{
    table_source::{inline_reference::ReferenceLabel, inline_stack::StackFile},
    table_window_request::{
        MarkdownTableRequestError, SemanticWork,
        ordinary_source::semantic::code::emphasis::EmphasisJob,
    },
};
use pulldown_cmark::global_frontier::{
    InlineCodeCursor, InlineCodeStep, InlineLinkStorageRequest, InlineLinkStorageStep,
    InlineReferenceKind, InlineReferenceResolution, InlineTextDecoder, VerifiedGlobalInlineBlock,
    VerifiedInlineCodeSpan, VerifiedInlineLink,
};

pub(super) enum HeadingSource {
    Atx(VerifiedGlobalLeafBlock),
    Setext(VerifiedGlobalInlineBlock),
}

impl HeadingSource {
    /// Takes ranges only from the original completed heading. ATX closing hashes
    /// and Setext underline bytes never enter its inline content.
    fn content(&self) -> Result<Range<usize>, Error> {
        match self {
            Self::Atx(leaf) => match leaf.details() {
                LeafBlockDetails::AtxHeading { trim_target, .. } => {
                    Ok(leaf.physical_start()..*trim_target)
                }
                _ => Err(Error::UnsupportedInline),
            },
            Self::Setext(block) => {
                Ok(block.physical_range().start..block.terminal_eol_range().start)
            }
        }
    }

    /// Preserve the parser options certified with this original ATX/Setext fact;
    /// changing options during projection could alter reference and delimiter meaning.
    fn options(&self) -> pulldown_cmark::Options {
        match self {
            Self::Atx(leaf) => leaf.options(),
            Self::Setext(block) => block.options(),
        }
    }

    fn scope(&self) -> Result<Range<usize>, Error> {
        // Emphasis matching starts at actual inline text. Physical prefixes do
        // not match delimiters but still occupy the mask's source coordinate space.
        // Keep these distinct responsibilities: skipping coordinate conversion
        // must not feed quote/list markers into visible text or the emphasis stack.
        match self {
            Self::Atx(leaf) => match leaf.details() {
                LeafBlockDetails::AtxHeading {
                    lexical_content,
                    trim_target,
                    ..
                } => Ok(lexical_content.start..*trim_target),
                _ => Err(Error::UnsupportedInline),
            },
            Self::Setext(block) => Ok(block.source_range().start..block.terminal_eol_range().start),
        }
    }

    /// Uses the fact's complete original source extent to bound admitted work,
    /// rather than a viewport edge or the current decoded-title length.
    fn source_end(&self) -> usize {
        match self {
            Self::Atx(leaf) => leaf.source_end(),
            Self::Setext(block) => block.source_end(),
        }
    }
}

struct Fill {
    range: Range<usize>,
    flags: u8,
}

/// One mask byte represents four source bytes: bit 0 hides, bit 1 marks code
/// literals. Hidden text always takes precedence over literal interpretation.
/// Creates anonymous storage only for the first actual marking fact. A plain
/// heading needs no second file, copied text, or whole-range zero initialization.
struct Mask {
    file: Option<File>,
    source: Range<usize>,
    pending: [Option<Fill>; 6],
    index: usize,
}

impl Mask {
    fn new(source: Range<usize>) -> Self {
        // Keep only the original fact's two offsets. File creation and sparse
        // extent setup wait for the first actual hidden/literal fact.
        // Thus even a giant plain heading allocates no length-proportional mask
        // memory and creates no empty output namespace.
        Self {
            file: None,
            source,
            pending: std::array::from_fn(|_| None),
            index: 0,
        }
    }

    /// Retains at most six ranges from one actual semantic fact. Its storage write
    /// must finish before accepting another fact that would replace these slots.
    fn begin(&mut self, fills: impl IntoIterator<Item = (Range<usize>, u8)>) -> Result<(), Error> {
        if self.busy() {
            // An unfinished storage write still owns these slots. Reject replacement
            // without losing its remaining original-source ranges.
            return Err(Error::UnsupportedInline);
        }
        // One proof may contain opener, body, closer, and both trim ranges. Six
        // fixed slots cover this fact only, never an inventory of the entire heading.
        // Do not overwrite unfinished ranges; later semantic reads wait for the
        // actual current file writes to complete.
        self.pending = std::array::from_fn(|_| None);
        self.index = 0;
        for (index, (range, flags)) in fills.into_iter().enumerate() {
            if index >= self.pending.len()
                || range.start < self.source.start
                || range.end > self.source.end
                || range.start > range.end
                || !(1..=2).contains(&flags)
            {
                return Err(Error::UnsupportedInline);
            }
            self.pending[index] = Some(Fill { range, flags });
        }
        Ok(())
    }

    fn busy(&self) -> bool {
        self.index < self.pending.len() && self.pending[self.index].is_some()
    }

    /// Updates at most one 1-KiB page per step. Sparse zero pages mean certified
    /// ordinary text, requiring no heading-sized RAM allocation or zero writes.
    fn advance(&mut self) -> Result<MarkdownHeadingAnchorWork, Error> {
        let mut work = MarkdownHeadingAnchorWork {
            structural_transitions: 1,
            ..Default::default()
        };
        let fill = self.pending[self.index]
            .as_mut()
            .ok_or(Error::UnsupportedInline)?;
        // Empty ranges are valid fact boundaries. Consume the slot without an
        // empty positioned write or an invented publication of new content.
        if fill.range.is_empty() {
            self.index += 1;
            return Ok(work);
        }
        if self.file.is_none() {
            // Acquire an unlinked file only after a nonempty certified mark exists.
            // A failed create/extent operation drops its local file before returning.
            let file = tempfile::tempfile().map_err(storage)?;
            file.set_len(self.source.len().div_ceil(4) as u64)
                .map_err(storage)?;
            self.file = Some(file);
            return Ok(work);
        }
        // Merge two-bit facts for four source bytes per mask byte. OR adds only
        // certified markings, and the hidden bit always overrides literal display.
        // For a cross-page range, advance only its written prefix. The next
        // quantum resumes this same fact before accepting another token.
        let first = fill.range.start - self.source.start;
        let last = (first / 4 + PAGE) * 4;
        let through = (fill.range.end - self.source.start).min(last);
        let offset = first / 4;
        let count = through.div_ceil(4) - offset;
        let mut bytes = [0; PAGE];
        let file = self.file.as_ref().unwrap();
        file.read_exact_at(&mut bytes[..count], offset as u64)
            .map_err(storage)?;
        for (index, byte) in bytes[..count].iter_mut().enumerate() {
            let start = (offset + index) * 4;
            let low = first.saturating_sub(start).min(4);
            let high = through.saturating_sub(start).min(4);
            let mask = ((1u16 << (high * 2)) - 1) & !((1u16 << (low * 2)) - 1);
            // Replicate each certified two-bit flag, then mask the actual source-byte intersection.
            *byte |= (fill.flags * 0x55) & mask as u8;
        }
        file.write_all_at(&bytes[..count], offset as u64)
            .map_err(storage)?;
        fill.range.start = self.source.start + through;
        work.storage_bytes = count * 2;
        work.structural_transitions += count + 8;
        if fill.range.is_empty() {
            self.index += 1;
        }
        Ok(work)
    }

    /// Reads two-bit facts for the current bounded source page. No file means
    /// plain text; never query or create a default backing file.
    fn flags(&self, start: usize, count: usize, output: &mut [u8]) -> Result<usize, Error> {
        // Unwritten sparse positions preserve ordinary-text meaning. Materialize
        // at most 65 mask bytes for this request's 256 source bytes.
        // Mask storage has separate ownership from document pages; cancellation
        // closes only this mask, never the user source or original file.
        output[..count].fill(0);
        let Some(file) = self.file.as_ref() else {
            return Ok(0);
        };
        let first = start - self.source.start;
        let offset = first / 4;
        let length = (first + count).div_ceil(4) - offset;
        let mut bytes = [0; 65];
        if length > bytes.len() {
            return Err(Error::UnsupportedInline);
        }
        file.read_exact_at(&mut bytes[..length], offset as u64)
            .map_err(storage)?;
        for (index, flag) in output[..count].iter_mut().enumerate() {
            let position = first + index;
            *flag = bytes[position / 4 - offset] >> ((position % 4) * 2) & 3;
        }
        Ok(length)
    }

    /// Discards pending metadata and explicitly closes only this anonymous mask.
    /// The source document remains owned and readable through its existing leases.
    fn close(&mut self) -> Result<(), Error> {
        self.pending = std::array::from_fn(|_| None);
        if let Some(file) = self.file.take() {
            let descriptor = file.into_raw_fd();
            // SAFETY: This owner exclusively owns the anonymous file. Transfer its
            // descriptor and close once; never retry a descriptor the OS may reuse.
            if unsafe { libc::close(descriptor) } != 0 {
                return Err(storage(io::Error::last_os_error()));
            }
        }
        Ok(())
    }
}

pub(super) struct HeadingProjection {
    initial: Option<HeadingSource>,
    cursor: Option<Box<InlineCodeCursor>>,
    source: Range<usize>,
    emphasis: EmphasisJob,
    stack: StackFile,
    quota: MarkdownTableWindowQuota,
    mask: Mask,
    storage: Option<InlineLinkStorageRequest>,
    link: Option<VerifiedInlineLink>,
    reference: Option<Box<ReferenceLabel>>,
    references: Option<Box<HeadingReferences>>,
    footnotes: Option<Box<HeadingFootnotes>>,
    decoded: bool,
    next: usize,
    decoder: InlineTextDecoder,
    flushing: bool,
    // Code bodies do not decode entities/backslashes. Carry at most three
    // incomplete UTF-8 bytes between actual source pages.
    literal: [u8; 4],
    literal_len: usize,
    // CRLF across pages still emits one visible space. Both bytes come from
    // certified text; a page boundary cannot invent a line-ending boundary.
    literal_after_cr: bool,
    normal_after_cr: bool,
    closed: bool,
}

/// Preserve actual semantic read/copy/parser costs while exposing source-tree
/// traversal as structural work at the original heading request's public boundary.
fn semantic(work: SemanticWork) -> MarkdownHeadingAnchorWork {
    MarkdownHeadingAnchorWork {
        source_bytes: work.source_bytes,
        copied_bytes: work.copied_source_bytes,
        parser_input_bytes: work.parser_input_bytes,
        metadata_bytes: work.metadata_bytes,
        structural_transitions: work.structural_transitions + work.source_tree_steps,
        ..Default::default()
    }
}

impl HeadingProjection {
    pub(super) fn new(source: HeadingSource) -> Result<Self, Error> {
        // Content is the physical source domain; scope is the original inline
        // matching domain. Hide prefixes once without copying prefix-stripped text.
        // The existing semantic-storage owner charges its fixed tail page and
        // actual new structural pages against real acquired capacity.
        let range = source.content()?;
        let scope = source.scope()?;
        let options = source.options();
        let fuel = source
            .source_end()
            .checked_mul(4096)
            .and_then(|n| n.checked_add(1024 * 1024))
            .ok_or(Error::ArithmeticOverflow)?;
        let mut mask = Mask::new(range.clone());
        if range.start < scope.start {
            mask.begin([(range.start..scope.start, 1)])?;
        }
        Ok(Self {
            initial: Some(source),
            cursor: None,
            source: range.clone(),
            emphasis: EmphasisJob::new(options, scope),
            stack: StackFile::new(),
            quota: MarkdownTableWindowQuota::new(1024, fuel),
            mask,
            storage: None,
            link: None,
            reference: None,
            references: None,
            footnotes: None,
            decoded: false,
            next: range.start,
            decoder: InlineTextDecoder::default(),
            flushing: false,
            literal: [0; 4],
            literal_len: 0,
            literal_after_cr: false,
            normal_after_cr: false,
            closed: false,
        })
    }

    /// One cursor exclusively handles source syntax. Pending mask writes or paused
    /// matching-stack work prevent reads from advancing to later source events.
    pub(super) fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        slugs: &mut Slugs,
        budget: usize,
    ) -> Result<(MarkdownHeadingAnchorWork, bool), Error> {
        let mut work = MarkdownHeadingAnchorWork {
            structural_transitions: 1,
            ..Default::default()
        };
        if self.closed {
            // Idempotent completion performs no source read or resource reacquisition.
            return Ok((work, true));
        }
        // The first quantum transfers the original fact to the shared cursor.
        // Construction never scans a giant heading or probes it with a short parser.
        // Account for the box's actual fixed size separately. All later lookahead,
        // storage, and reference requests still belong to this same cursor.
        if let Some(source) = self.initial.take() {
            let fuel = document
                .byte_len()
                .checked_mul(4096)
                .and_then(|n| n.checked_add(1024 * 1024))
                .ok_or(Error::ArithmeticOverflow)?;
            let limits = InlinePlainLimits {
                max_byte_work: fuel,
                max_transitions: fuel,
            };
            let cursor = match source {
                HeadingSource::Atx(leaf) => leaf.heading_code_cursor(limits),
                HeadingSource::Setext(block) => block.into_heading_code_cursor(limits),
            }
            .map_err(Error::Global)?;
            let capacity = std::mem::size_of::<InlineCodeCursor>();
            self.quota
                .admit_metadata(capacity)
                .map_err(|error| Error::Semantic(MarkdownTableRequestError::Window(error)))?;
            work.metadata_bytes = capacity;
            self.cursor = Some(Box::new(cursor));
            return Ok((work, false));
        }
        // Finish file facts before taking another source event; resume paused
        // matching-stack work first too. No source lease crosses these I/O steps.
        // This order prevents later proofs overwriting pending ranges and keeps
        // each quantum's actual resource work at one accounting boundary.
        if self.mask.busy() {
            return self.mask.advance().map(|work| (work, false));
        }
        if self.emphasis.busy() {
            let (measured, span) = self
                .emphasis
                .advance(&self.quota, budget, &mut self.stack)
                .map_err(Error::Semantic)?;
            // The original emphasis job certifies matched spans. Hide only actual
            // delimiters; body pages are leased later during source-ordered output.
            // Unmatched runs never gain hidden authority; literal underscores and
            // similar text remain exactly as the original source requires.
            if let Some(span) = span {
                self.mask.begin([
                    (span.whole.start..span.body.start, 1),
                    (span.body.end..span.whole.end, 1),
                ])?;
            }
            return Ok((semantic(measured), false));
        }
        // Begin saves the outer emphasis head before entering a complete label.
        // A required stack flush retains this same proof rather than repeating Begin.
        // URL/title ranges do not enter label emphasis matching or become
        // another copied representation of visible heading text.
        if let Some(proof) = self.link.take() {
            let (measured, entered) = self
                .emphasis
                .enter_link(&proof, &self.quota, budget, &mut self.stack)
                .map_err(Error::Semantic)?;
            if !entered {
                self.link = Some(proof);
            }
            return Ok((semantic(measured), false));
        }
        // Real stack-file replies resume only the paused operation. Progress is
        // not a completed append/read and cannot publish a link prematurely.
        // Metadata reflects actual tail-page allocation. Replies already carry
        // conversion/flush costs; acceptance forwards them without double charging.
        if let Some(request) = self.storage.as_ref() {
            let (step, metadata) = self
                .stack
                .storage(request, &self.quota, budget)
                .map_err(Error::Semantic)?;
            let ready = matches!(&step, InlineLinkStorageStep::Ready { .. });
            let measured = self
                .cursor
                .as_mut()
                .unwrap()
                .accept_storage(
                    request,
                    step,
                    GlobalSourceAuthority {
                        current_revision: document.generation(),
                        cancelled: false,
                    },
                    budget,
                )
                .map_err(Error::Global)?;
            if ready {
                self.storage = None;
            }
            work.structural_transitions += measured.transitions;
            work.metadata_bytes += metadata;
            return Ok((work, false));
        }
        if self.reference.is_some() {
            // A paused identifier must receive its actual found/missing answer before
            // the original inline cursor can consume another source event.
            return self.advance_reference(document, budget);
        }
        if self.decoded {
            // Visible output is legal only after all source-wide inline decisions close.
            return self.render(document, slugs, budget);
        }
        let cursor = self.cursor.as_mut().ok_or(Error::UnsupportedInline)?;
        // Code/HTML/link lookahead changes the next required source location.
        // Lease cursor.next_byte instead of advancing an assumed sequential offset.
        // Finish this lease before later file work or error propagation. The
        // document retains its original mmap health and reclamation responsibility.
        let frame = document.table_source_frame(cursor.next_byte(), 256)?;
        let actual_read = frame.bytes.len();
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
                budget - 128,
            )
            .map_err(Error::Global);
        frame.finish()?;
        let step = step?;
        let measured = match &step {
            InlineCodeStep::Progress { work }
            | InlineCodeStep::NeedInput { work }
            | InlineCodeStep::Span { work, .. }
            | InlineCodeStep::Prefix { work, .. }
            | InlineCodeStep::Emphasis { work, .. }
            | InlineCodeStep::Link { work, .. }
            | InlineCodeStep::Storage { work, .. }
            | InlineCodeStep::Reference { work, .. }
            | InlineCodeStep::LinkEnd { work, .. }
            | InlineCodeStep::Proven { work, .. }
            | InlineCodeStep::NeedsGeneralInline { work } => *work,
        };
        work.source_bytes = actual_read;
        work.structural_transitions += measured.transitions;
        // Additional facts describe resolved footnotes, original trailing line
        // whitespace, or completed HTML/autolinks, at most one pending fact per step.
        // The same source-ordered cursor certifies each; styles, a first '>', or
        // text searching cannot manufacture their visible ranges.
        if let Some(footnote) = cursor.take_heading_footnote() {
            self.mask.begin([(footnote.source_range(), 1)])?;
        }
        if let Some(trim) = cursor.take_heading_line_trim() {
            self.mask.begin([(trim.source_range(), 1)])?;
        }
        if let Some(html) = cursor.take_heading_html() {
            let range = html.source_range();
            if html.is_autolink() {
                // An autolink displays its original literal URL. Its '&amp;' spelling
                // must not undergo ordinary-text entity decoding again.
                // Hide both angle brackets; a fixed UTF-8 carry reads the interior
                // across real source-page boundaries.

                self.mask.begin([
                    (range.start + 1..range.end - 1, 2),
                    (range.start..range.start + 1, 1),
                    (range.end - 1..range.end, 1),
                ])?;
            } else {
                self.mask.begin([(range, 1)])?;
            }
        }
        match step {
            InlineCodeStep::Span { proof, .. } => {
                work.source_bytes += self.code(document, proof)?;
            }
            InlineCodeStep::Prefix { proof, .. } => self.mask.begin([(proof.source_range(), 1)])?,
            InlineCodeStep::Emphasis { run, .. } => {
                self.emphasis.start(run).map_err(Error::Semantic)?
            }
            InlineCodeStep::Link { proof, .. } => {
                // Keep the certified label visible while hiding only its surrounding
                // syntax/destination; emphasis enters the original label separately.
                let source = proof.source_range();
                let label = proof.label_range();
                self.mask
                    .begin([(source.start..label.start, 1), (label.end..source.end, 1)])?;
                self.link = Some(proof);
            }
            InlineCodeStep::LinkEnd { source, label, .. } => {
                // Restore the matching outer emphasis scope only when this same
                // source link actually closes; the URL/title never joined that stack.
                let measured = self
                    .emphasis
                    .leave_link(&source, &label, budget, &mut self.stack)
                    .map_err(Error::Semantic)?;
                work.structural_transitions += measured.structural_transitions;
            }
            InlineCodeStep::Storage { request, .. } => self.storage = Some(request),
            InlineCodeStep::Reference { request, .. } => {
                self.reference = Some(Box::new(ReferenceLabel::new(request)))
            }
            InlineCodeStep::Proven { .. } => {
                // The actual inline domain has closed, so later text cannot change
                // delimiter/link decisions. Only now may visible output begin.
                // Explicitly close external matching history immediately. Rendering
                // retains only the mask, slug file, and fixed bounded pages.

                self.decoded = true;
                self.cursor = None;
                self.emphasis.finish();
                self.stack.close().map_err(Error::Semantic)?; // Return syntax storage at match completion, not at eventual navigation.
            }
            InlineCodeStep::NeedsGeneralInline { .. } => return Err(Error::UnsupportedInline),
            _ => {}
        }
        Ok((work, false))
    }

    /// Marks literal code and certified trimming separately. Inspect at most two
    /// bytes at each original body edge to determine actual CRLF width.
    fn code(
        &mut self,
        document: &MarkdownDocumentBuffer,
        proof: VerifiedInlineCodeSpan,
    ) -> Result<usize, Error> {
        let whole = proof.source_range();
        let body = proof.raw_body_range();
        let mut start = body.start;
        let mut end = body.end;
        if proof.trims_boundary_spaces() {
            // The original code matcher certifies trimming. Inspect at most two
            // bytes per edge to distinguish actual CRLF from one normalized space.
            // All-space code never enters this branch; its visible whitespace must
            // not be removed by an independent trimming policy.

            start += self.eol_width(document, body.start, (body.start + 2).min(body.end))?;
            let tail = body.end.saturating_sub(2).max(body.start);
            let mut bytes = [0; 2];
            read_source(document, tail, &mut bytes[..body.end - tail])?;
            end -= if &bytes[..body.end - tail] == b"\r\n" {
                2
            } else {
                1
            };
        }
        self.mask.begin([
            (body.clone(), 2),
            (whole.start..body.start, 1),
            (body.end..whole.end, 1),
            (body.start..start, 1),
            (end..body.end, 1),
        ])?;
        Ok(if proof.trims_boundary_spaces() {
            body.len().min(2) * 2
        } else {
            0
        })
    }

    /// Distinguishes a real two-byte CRLF edge from one byte normalized as space,
    /// borrowing no more than the caller's already bounded two-byte source extent.
    fn eol_width(
        &self,
        document: &MarkdownDocumentBuffer,
        start: usize,
        end: usize,
    ) -> Result<usize, Error> {
        let mut bytes = [0; 2];
        read_source(document, start, &mut bytes[..end - start])?;
        Ok(if &bytes[..end - start] == b"\r\n" {
            2
        } else {
            1
        })
    }

    /// Resolves one current certified identifier through the original global
    /// frontier and bounded definition parser. Missing requires real source EOF.
    fn advance_reference(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<(MarkdownHeadingAnchorWork, bool), Error> {
        let reference = self.reference.as_mut().unwrap();
        if reference.label().is_none() {
            // Finish normalized-label preparation first. Raw or partial identifiers
            // cannot authorize either a definition lookup or a missing answer.
            return reference
                .advance(document, &self.quota, budget)
                .map(|work| (semantic(work), false))
                .map_err(Error::Semantic);
        }
        if reference.request.kind() == InlineReferenceKind::Footnote {
            // Footnotes ask only about existence; ordinary references resolve actual
            // URL/title values. A fake URL cannot substitute for either answer.
            // While querying, retain the original cursor's paused identity. Resume
            // this same request only after its found/missing answer is ready.

            if self.footnotes.is_none() {
                let mut footnotes = HeadingFootnotes::new(self.quota.clone());
                footnotes.lookup(reference.label().unwrap());
                if let Some(error) = footnotes.take_failure() {
                    return Err(error);
                }
                self.footnotes = Some(Box::new(footnotes));
                return Ok((
                    MarkdownHeadingAnchorWork {
                        structural_transitions: 1,
                        ..Default::default()
                    },
                    false,
                ));
            }
            let (work, complete) = self.footnotes.as_mut().unwrap().advance(document, budget)?;
            if complete {
                let found = self
                    .footnotes
                    .as_mut()
                    .unwrap()
                    .lookup(reference.label().unwrap())
                    == Some(true);
                self.cursor
                    .as_mut()
                    .unwrap()
                    .accept_reference(
                        &reference.request,
                        if found {
                            InlineReferenceResolution::Resolved
                        } else {
                            InlineReferenceResolution::Missing
                        },
                        GlobalSourceAuthority {
                            current_revision: document.generation(),
                            cancelled: false,
                        },
                    )
                    .map_err(Error::Global)?;
                self.footnotes = None;
                self.reference = None;
            }
            return Ok((work, false));
        }
        if self.references.is_none() {
            // Admit only this actual normalized label. The existing global frontier
            // searches definitions without building a document-wide reference map.
            let mut queries = MarkdownTableReferenceQueries::for_batch(Some(self.quota.clone()));
            queries
                .collect_verified_label(reference.label().unwrap())
                .map_err(|error| Error::Semantic(MarkdownTableRequestError::Window(error)))?;
            self.references = Some(Box::new(HeadingReferences::new(document, queries)?));
            return Ok((
                MarkdownHeadingAnchorWork {
                    structural_transitions: 1,
                    ..Default::default()
                },
                false,
            ));
        }
        let (work, complete) = self
            .references
            .as_mut()
            .unwrap()
            .advance(document, budget)?;
        if complete {
            // Resume the exact paused request after all needed definitions or real
            // EOF complete. A local absence cannot be converted into a missing link.
            let resolved = self
                .references
                .as_ref()
                .unwrap()
                .resolve(reference.label().unwrap())
                .is_some();
            self.cursor
                .as_mut()
                .unwrap()
                .accept_reference(
                    &reference.request,
                    if resolved {
                        InlineReferenceResolution::Resolved
                    } else {
                        InlineReferenceResolution::Missing
                    },
                    GlobalSourceAuthority {
                        current_revision: document.generation(),
                        cancelled: false,
                    },
                )
                .map_err(Error::Global)?;
            self.references = None;
            self.reference = None;
        }
        Ok((work, false))
    }

    /// Emits source-ordered visible text only after semantics close. A completed
    /// slug base already longer than the bounded target can stop further output.
    fn render(
        &mut self,
        document: &MarkdownDocumentBuffer,
        slugs: &mut Slugs,
        budget: usize,
    ) -> Result<(MarkdownHeadingAnchorWork, bool), Error> {
        let mut work = MarkdownHeadingAnchorWork {
            structural_transitions: 1,
            ..Default::default()
        };
        // Semantics are complete. Once the slug base exceeds target length,
        // later uniqueness suffixes only lengthen it and cannot produce a match.
        // End remaining output and close storage, but never apply this pruning
        // during an earlier literal scan whose semantic decisions remain open.
        if slugs.irrelevant {
            self.close()?;
            return Ok((work, true));
        }
        // Decoder carry belongs to this ordinary-text domain. Flush it before
        // hidden/code boundaries so markup cannot splice fragments into a new entity.
        // Actual source end still waits for the final complete UTF-8 output.
        // Neither empty output nor a short chunk proves completion.
        if self.flushing || self.next == self.source.end {
            let mut output = [0; 256];
            let step = self
                .decoder
                .feed(&[], true, &mut output, budget)
                .map_err(Error::Global)?;
            let text = std::str::from_utf8(&output[..step.written])
                .map_err(|_| Error::UnsupportedInline)?;
            let (writes, copied) = slugs.append_visible(text)?;
            work.storage_bytes = writes;
            work.copied_bytes = copied;
            work.structural_transitions += step.transitions;
            if step.complete {
                self.flushing = false;
                self.decoder = InlineTextDecoder::default();
                if self.next == self.source.end {
                    self.close()?;
                    return Ok((work, true));
                }
            }
            return Ok((work, false));
        }
        let count = (self.source.end - self.next).min(256);
        let mut flags = [0; 256];
        work.storage_bytes += self.mask.flags(self.next, count, &mut flags)?;
        let flag = flags[0];
        let mut group = flags[..count]
            .iter()
            .position(|&other| other != flag)
            .unwrap_or(count);
        if flag & 1 != 0 {
            // A hidden boundary ends the preceding ordinary-text domain, preventing
            // '&am' and 'p;' separated by markup from becoming a new entity.
            self.next += group;
            self.flushing = true;
            self.literal_after_cr = false;
            self.normal_after_cr = false;
            if self.literal_len != 0 {
                return Err(Error::UnsupportedInline);
            }
            return Ok((work, false));
        }
        let mut bytes = [0; 260];
        read_source(document, self.next, &mut bytes[..group])?;
        work.source_bytes = group;
        work.copied_bytes = group;
        if flag & 2 != 0 {
            // Original code-span rules normalize real CR/LF to spaces but never
            // decode entities. A fixed carry joins incomplete UTF-8 tails.
            bytes.copy_within(..group, self.literal_len);
            bytes[..self.literal_len].copy_from_slice(&self.literal[..self.literal_len]);
            let length = group + self.literal_len;
            let valid = match std::str::from_utf8(&bytes[..length]) {
                Ok(_) => length,
                Err(error) if error.error_len().is_none() => error.valid_up_to(),
                Err(_) => return Err(Error::UnsupportedInline),
            };
            self.literal_len = length - valid;
            self.literal[..self.literal_len].copy_from_slice(&bytes[valid..length]);
            let text =
                std::str::from_utf8(&bytes[..valid]).map_err(|_| Error::UnsupportedInline)?;
            // CR, LF, and CRLF each yield one space under code-span rules. Keep
            // only continuation state, never a copy of the complete code span.
            let mut text_bytes = String::with_capacity(valid);
            for scalar in text.chars() {
                if scalar == '\n' && self.literal_after_cr {
                    self.literal_after_cr = false;
                    continue;
                }
                self.literal_after_cr = scalar == '\r';
                text_bytes.push(if matches!(scalar, '\r' | '\n') {
                    ' '
                } else {
                    scalar
                });
            }
            let (writes, copied) = slugs.append_visible(&text_bytes)?;
            self.next += group;
            work.storage_bytes += writes;
            work.copied_bytes += copied;
            return Ok((work, false));
        }
        // Certified domain boundaries must align to UTF-8 scalars. Code carry
        // cannot leak into ordinary text or a different code span.
        // Fail through the original cleanup chain; replacement characters must
        // not hide source-coordinate or decoding errors.
        if self.literal_len != 0 {
            // Another semantic domain cannot borrow an incomplete code scalar.
            // Refuse publication rather than substitute a replacement character.
            return Err(Error::UnsupportedInline);
        }
        self.literal_after_cr = false;
        if self.normal_after_cr && bytes[0] == b'\n' {
            // The preceding actual CR already emitted a space. Consume its paired
            // LF without emitting a second space.
            self.next += 1;
            self.normal_after_cr = false;
            return Ok((work, false));
        }
        self.normal_after_cr = false;
        if let Some(end) = bytes[..group]
            .iter()
            .position(|byte| matches!(byte, b'\r' | b'\n'))
        {
            // Shorten this page only at an actual EOL. Input/output replacement length
            // stays equal, and decoder consumption still counts canonical source bytes.
            group = end + 1;
            bytes[end] = b' ';
        }
        let real_end = if group < count || self.next + group == self.source.end {
            true
        } else {
            let mut next = [0];
            self.mask.flags(self.next + group, 1, &mut next)?;
            next[0] != 0
        };
        let mut output = [0; 256];
        let step = self
            .decoder
            .feed(&bytes[..group], real_end, &mut output, budget)
            .map_err(Error::Global)?;
        // A full output page may stop consumption early. Only consuming the
        // actual final CR grants responsibility for normalizing the next LF.
        if step.consumed == group {
            let mut last = [0];
            read_source(document, self.next + group - 1, &mut last)?;
            work.source_bytes += 1;
            self.normal_after_cr = last[0] == b'\r';
        }
        self.next += step.consumed;
        self.flushing = real_end && !step.complete;
        if step.complete {
            self.decoder = InlineTextDecoder::default();
        }
        let text =
            std::str::from_utf8(&output[..step.written]).map_err(|_| Error::UnsupportedInline)?;
        let (writes, copied) = slugs.append_visible(text)?;
        work.storage_bytes += writes;
        work.copied_bytes += copied;
        work.structural_transitions += step.transitions;
        Ok((work, false))
    }

    /// Attempts every owned resource release. Preserve both close failures rather
    /// than skipping another descriptor after the first error.
    pub(super) fn close(&mut self) -> Result<(), Error> {
        self.initial = None;
        self.cursor = None;
        self.reference = None;
        self.references = None;
        self.footnotes = None;
        self.storage = None;
        self.link = None;
        self.closed = true;
        let stack = self.stack.close().map_err(Error::Semantic);
        let mask = self.mask.close();
        match (stack, mask) {
            (Err(primary), Err(cleanup)) => Err(Error::Cleanup {
                primary: Arc::new(primary),
                cleanup: Arc::new(cleanup),
            }),
            (Err(error), _) | (_, Err(error)) => Err(error),
            _ => Ok(()),
        }
    }
}

/// Joins at most 260 bytes for the current logical chunk. Finish every real
/// lease separately; UTF-8 may cross pieces without manufacturing source EOF.
fn read_source(
    document: &MarkdownDocumentBuffer,
    start: usize,
    output: &mut [u8],
) -> Result<(), Error> {
    let mut copied = 0;
    while copied < output.len() {
        let frame = document.table_source_frame(start + copied, output.len() - copied)?;
        let count = frame.bytes.len();
        output[copied..copied + count].copy_from_slice(&frame.bytes);
        copied += count;
        frame.finish()?;
        if count == 0 {
            // A nonempty pending range needs real bytes. Neither an empty lease nor
            // the end of a physical piece is a successful logical end-of-source.
            return Err(Error::UnsupportedInline);
        }
    }
    Ok(())
}
