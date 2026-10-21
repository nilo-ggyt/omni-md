//! Finite Plain/Setext and verified inline semantics within the original source owner; unsupported syntax and coverage holes stay Pending.

use super::*;
use crate::MarkdownSourceLink;
use crate::markdown_document_buffer::table_source::MarkdownTableUnderlineRequest;
use crate::markdown_viewport_presentation::parser::markdown_selection_intersects;
use crate::markdown_viewport_presentation::table_window::MarkdownTableWindowQuota;
use crate::markdown_viewport_presentation::{
    MarkdownPresentationQuery, MarkdownPresentationRun, MarkdownPresentationStyles,
};
use pulldown_cmark::global_frontier::{
    InlinePlainCursor, InlinePlainLimits, InlinePlainStep, VerifiedInlinePlainProof,
    VerifiedInlineSemanticSeed,
};
mod bounded_inline;
use bounded_inline::BoundedInline;
pub(in crate::markdown_document_buffer) mod code;
use code::CodeJob;
pub(super) use code::{PrefixDecoration, SpanProof};

const PLAIN_CONTROL: usize = 128;
const MAX_PLAIN_RUNS: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownOrdinaryPendingReason {
    MissingClosedBlock,
    IncompleteCoverage,
    NeedsGeneralInline,
    NonRootScope,
}

/// Retain the original paired seed; physical lines and the semantic job share one Rc instead of copying scope per line.
pub(super) struct OrdinaryBlockRecord {
    pub(super) seed: VerifiedInlineSemanticSeed,
}

/// Two coordinates for one actually projected endpoint, used only during capability construction rather than as another text index.
#[cfg(feature = "paged-line-compose")]
pub(super) struct PlainPrefixEnd {
    pub(super) byte: usize,
    pub(super) utf16: usize,
}

impl OrdinaryBlockRecord {
    /// Container paragraphs need original Origin before inline processing; Root and Setext retain their own complete-block contracts.
    pub(super) fn is_scoped_paragraph(&self) -> bool {
        self.seed.scope_depth() > 1 && self.seed.kind() == InlineBlockKind::Paragraph
    }
}

/// Share original publication identity between source facts and finite semantics; SourcesOnly/Pending do not prove default styles.
pub enum MarkdownOrdinarySemanticState {
    SourcesOnly,
    Pending(MarkdownOrdinaryPendingReason),
    PlainReady(Box<VerifiedMarkdownPlainViewport>),
    LeafPending(MarkdownRootLeafPendingReason),
    /// Prove only listed actual closed-block intersections; holes in the original window remain unknown rather than default-styled.
    RootLeafReady(Box<VerifiedMarkdownRootLeafViewport>),
}

impl MarkdownOrdinarySemanticState {
    /// Old results may coexist with successors; all runs from both actual Ready results occupy the original joint inventory.
    pub(super) fn run_count(&self) -> usize {
        match self {
            Self::PlainReady(ready) => ready.run_count(),
            Self::RootLeafReady(ready) => ready.run_count(),
            Self::SourcesOnly | Self::Pending(_) | Self::LeafPending(_) => 0,
        }
    }

    /// Charge only retained Boxes/pages to exported results; reservation alone does not invent semantic payload for Pending.
    pub(super) fn retained_metadata(&self) -> usize {
        match self {
            Self::PlainReady(ready) => {
                plain_publication_metadata()
                    + ready.rich_runs.capacity()
                        * std::mem::size_of::<Option<MarkdownPresentationRun>>()
                    + ready.links.capacity() * std::mem::size_of::<MarkdownSourceLink>()
                    + ready
                        .links
                        .iter()
                        .map(|link| link.destination.retained_metadata())
                        .sum::<usize>()
            }
            Self::RootLeafReady(ready) => ready.retained_metadata(),
            Self::SourcesOnly | Self::Pending(_) | Self::LeafPending(_) => 0,
        }
    }
}

/// Check original source, complete selection, and publication before each query; read-only results do not authorize request continuation.
pub struct VerifiedMarkdownPlainViewport {
    source: MarkdownTableSourceIdentity,
    context_revision: u64,
    selection: MarkdownTextSelection,
    publication: Weak<SourcePublication>,
    requested: Range<usize>,
    runs: [Option<MarkdownPresentationRun>; MAX_PLAIN_RUNS],
    len: usize,
    block: Rc<OrdinaryBlockRecord>,
    rich_runs: Vec<Option<MarkdownPresentationRun>>,
    links: Vec<MarkdownSourceLink>,
}

/// Admit Box payload through original source-publication reservation; the enum handle is already included in its parent structure.
pub(super) fn plain_publication_metadata() -> usize {
    std::mem::size_of::<VerifiedMarkdownPlainViewport>()
}

impl VerifiedMarkdownPlainViewport {
    pub(super) fn run_count(&self) -> usize {
        self.len
    }
    fn check(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<(), MarkdownTableRequestError> {
        check_authority(
            &self.source,
            self.context_revision,
            self.selection,
            document,
            authority,
        )?;
        // Isolated semantic metadata cannot recover publication identity from an equal generation after the original result is dropped or retired.
        self.publication
            .upgrade()
            .ok_or(MarkdownTableRequestError::PublicationSuperseded)?
            .require_live()
    }

    pub fn coverage_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<&Range<usize>, MarkdownTableRequestError> {
        self.check(document, authority)?;
        Ok(&self.requested)
    }

    pub fn block_source_range_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<Range<usize>, MarkdownTableRequestError> {
        self.check(document, authority)?;
        Ok(self.block.seed.source_range())
    }

    /// Some(default) differs from out-of-coverage None; never manufacture empty styles for a gap or Pending result.
    pub fn query_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
        index: usize,
    ) -> Result<Option<MarkdownPresentationQuery>, MarkdownTableRequestError> {
        self.check(document, authority)?;
        if !self.requested.contains(&index) {
            return Ok(None);
        }
        // Search the fixed base slots and quota-bounded rich runs, not source text or every block.
        // Only the base array has a 16-run limit; rich output has its own admitted capacity, including explicit default runs.
        Ok(self.runs[..self.len.min(MAX_PLAIN_RUNS)]
            .iter()
            .chain(self.rich_runs.iter())
            .flatten()
            .find(|run| run.utf16_range.contains(&index))
            .map(|run| MarkdownPresentationQuery {
                utf16_range: run.utf16_range.clone(),
                styles: run.styles,
            }))
    }

    pub fn runs_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<impl Iterator<Item = &MarkdownPresentationRun>, MarkdownTableRequestError> {
        self.check(document, authority)?;
        Ok(self.runs[..self.len.min(MAX_PLAIN_RUNS)]
            .iter()
            .chain(self.rich_runs.iter())
            .flatten())
    }

    /// Link hits retain original Source/complete-selection/publication checks; a style bit alone grants no URL authority.
    pub fn link_at_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
        index: usize,
    ) -> Result<Option<&MarkdownSourceLink>, MarkdownTableRequestError> {
        self.check(document, authority)?;
        if !self.requested.contains(&index) {
            return Err(MarkdownTableRequestError::Window(
                MarkdownTableWindowError::ContextPending,
            ));
        }
        Ok(self
            .links
            .iter()
            .find(|link| link.label_utf16_range.contains(&index)))
    }
}

#[derive(Clone, Copy)]
enum SemanticPhase {
    Bounds,
    AwaitSource,
    Proof,
    AtxBounded,
    ScopedBounded,
    BoundedInline,
    Code,
    Coordinates,
    Underline,
    Build,
    Done,
}

/// State belongs to the original request; forward movement in one block retains its prefix cursor without a new Global or renewed cumulative fuel.
pub(super) struct PlainSemanticJob {
    requested: Range<usize>,
    cell: Option<VerifiedMarkdownTableCellSource>,
    selection: MarkdownTextSelection,
    positions: [PositionLocator; 4],
    position_index: usize,
    byte_coverage: Option<Range<usize>>,
    selection_bytes: Range<usize>,
    block: Option<Rc<OrdinaryBlockRecord>>,
    // Original Origin admits only the same block's scoped entry; the sole Code owner incrementally proves prefixes and inline pairs.
    line: Option<BlockOriginLine>,
    cursor: Option<InlinePlainCursor>,
    proof: Option<VerifiedInlinePlainProof>,
    projection: Option<Coordinates>,
    projected: Option<[usize; 4]>,
    underline: Option<MarkdownTableUnderlineRequest>,
    runs: [Option<MarkdownPresentationRun>; MAX_PLAIN_RUNS],
    len: usize,
    run_next: usize,
    underline_index: usize,
    pending: Option<MarkdownOrdinaryPendingReason>,
    saw_intersection: bool,
    phase: SemanticPhase,
    bounded: Option<Box<BoundedInline>>,
    code: Option<Box<CodeJob>>,
    quota: MarkdownTableWindowQuota,
}

impl PlainSemanticJob {
    /// Copy only a real cell seed's fixed scalars/shared scope, never its text or a raw range masquerading as default-style proof.
    pub(super) fn for_cell(
        cell: &VerifiedMarkdownTableCellSource,
        selection: MarkdownTextSelection,
        selection_bytes: Option<Range<usize>>,
        underline: Option<MarkdownTableUnderlineRequest>,
        quota: MarkdownTableWindowQuota,
    ) -> Result<Self, MarkdownTableRequestError> {
        quota
            .admit_metadata(
                // Self is embedded in the admitted sole CellInlineJob slot; allocate only the original block's independent Rc here.
                std::mem::size_of::<OrdinaryBlockRecord>() + 2 * std::mem::size_of::<usize>(),
            )
            .map_err(MarkdownTableRequestError::Window)?;
        let mut job = Self::new(cell.range.clone(), selection, quota);
        job.block = Some(Rc::new(OrdinaryBlockRecord {
            seed: cell
                .inline_seed
                .as_deref()
                .expect("非空真实格持原 Scanner 签发的 seed")
                .clone(),
        }));
        job.cell = Some(cell.clone());
        job.underline = underline;
        job.saw_intersection = true;
        if let Some(selection_bytes) = selection_bytes {
            // The scanner issued both this cell's UTF-16 range and actual trimmed-byte seed, while the parent already located
            // the same complete selection. Continue semantics directly without rereading four source prefixes for every cell.
            job.byte_coverage = Some(
                job.block
                    .as_ref()
                    .expect("非空真实格已持原 seed")
                    .seed
                    .physical_range(),
            );
            job.selection_bytes = selection_bytes;
            job.phase = SemanticPhase::AwaitSource;
        }
        Ok(job)
    }
    pub(super) fn new(
        requested: Range<usize>,
        selection: MarkdownTextSelection,
        quota: MarkdownTableWindowQuota,
    ) -> Self {
        let ordered = ordered_selection(selection);
        Self {
            positions: [
                PositionLocator::new(requested.start, false),
                PositionLocator::new(requested.end, true),
                PositionLocator::new(ordered.start, false),
                PositionLocator::new(ordered.end, true),
            ],
            run_next: requested.start,
            requested,
            cell: None,
            selection,
            position_index: 0,
            byte_coverage: None,
            selection_bytes: 0..0,
            block: None,
            line: None,
            cursor: None,
            proof: None,
            projection: None,
            projected: None,
            underline: None,
            runs: std::array::from_fn(|_| None),
            len: 0,
            underline_index: 0,
            pending: None,
            saw_intersection: false,
            phase: SemanticPhase::Bounds,
            bounded: None,
            code: None,
            quota,
        }
    }

    /// The same RootLeaf already measured four endpoints and its closed-block intersection; accept only its original seed and sole underline owner.
    /// Do not accept caller syntax guesses, relocate distant selection, or create another source-wide matching request.
    pub(super) fn for_closed_block(
        requested: Range<usize>,
        selection: MarkdownTextSelection,
        byte_coverage: Range<usize>,
        selection_bytes: Range<usize>,
        block: &Rc<OrdinaryBlockRecord>,
        underline: MarkdownTableUnderlineRequest,
        quota: MarkdownTableWindowQuota,
    ) -> Result<Self, MarkdownTableRequestError> {
        let physical = block.seed.physical_range();
        if requested.is_empty()
            || byte_coverage.is_empty()
            || physical.start > byte_coverage.start
            || byte_coverage.end > physical.end
        {
            // The actual intersection must lie inside the original closed fact; neighboring lines or request envelopes cannot certify Plain.
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        let mut job = Self::new(requested, selection, quota);
        job.byte_coverage = Some(byte_coverage);
        job.selection_bytes = selection_bytes;
        job.block = Some(Rc::clone(block));
        job.saw_intersection = true;
        job.underline = Some(underline);
        job.phase = SemanticPhase::AwaitSource;
        Ok(job)
    }

    /// The original SourceRequest proved physical/prefix/content/EOL boundaries through the same Global/PrefixCursor.
    /// Transfer fixed Origin identity and create one Code owner per block instead of rescanning multiline emphasis per physical line.
    pub(super) fn for_closed_origin(
        requested: Range<usize>,
        selection: MarkdownTextSelection,
        byte_coverage: Range<usize>,
        selection_bytes: Range<usize>,
        line: &VerifiedMarkdownOrdinaryLine,
        underline: MarkdownTableUnderlineRequest,
        quota: MarkdownTableWindowQuota,
    ) -> Result<Self, MarkdownTableRequestError> {
        let mut job = Self::for_closed_block(
            requested,
            selection,
            byte_coverage,
            selection_bytes,
            line.semantic_block
                .as_ref()
                .expect("语义行必须持原配对 seed"),
            underline,
            quota,
        )?;
        job.line = Some(line.origin.clone());
        Ok(job)
    }

    /// Obtain finite ATX parse authority only from an actual closed leaf; the parent first completes original container capture/prefix work.
    pub(super) fn supports_closed_atx(leaf: &VerifiedGlobalLeafBlock) -> bool {
        BoundedInline::supports_atx(leaf)
    }

    /// Reuse the parent's sole semantic slot for ATX; initialize fixed fields now and admit text/Parser workspace in a separate later step.
    pub(super) fn for_closed_atx(
        requested: Range<usize>,
        selection: MarkdownTextSelection,
        byte_coverage: Range<usize>,
        selection_bytes: Range<usize>,
        leaf: &VerifiedGlobalLeafBlock,
        underline: Option<MarkdownTableUnderlineRequest>,
        quota: MarkdownTableWindowQuota,
    ) -> Result<Self, MarkdownTableRequestError> {
        if !Self::supports_closed_atx(leaf)
            || requested.is_empty()
            || byte_coverage.is_empty()
            || leaf.physical_start() > byte_coverage.start
            || byte_coverage.end > leaf.source_range().end
        {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        let mut job = Self::new(requested, selection, quota);
        job.byte_coverage = Some(byte_coverage);
        job.selection_bytes = selection_bytes;
        job.underline = underline;
        job.phase = SemanticPhase::AtxBounded;
        Ok(job)
    }

    /// The leaf owner supplies the same actual ticket each step; first allocate the bounded job, then follow the existing Parser/matching/Build chain.
    pub(super) fn advance_atx_after_sources(
        &mut self,
        document: &MarkdownDocumentBuffer,
        leaf: &VerifiedGlobalLeafBlock,
        budget: usize,
        limits: (usize, usize),
        global: Option<&mut GlobalFrontier>,
    ) -> Result<(SemanticWork, usize), MarkdownTableRequestError> {
        if matches!(self.phase, SemanticPhase::AtxBounded) {
            self.bounded = Some(BoundedInline::for_atx(leaf, &self.quota)?);
            self.phase = SemanticPhase::BoundedInline;
            return Ok((
                SemanticWork {
                    metadata_bytes: BoundedInline::atx_metadata(),
                    structural_transitions: PLAIN_CONTROL,
                    ..Default::default()
                },
                0,
            ));
        }
        self.advance_after_sources(document, budget, limits, global)
    }

    /// The parent reserved this whole fixed slot; a new block fills active fields without separately allocating its empty embedded cursor workspace.
    pub(super) fn closed_block_metadata() -> usize {
        std::mem::size_of::<Self>() - std::mem::size_of::<Option<InlinePlainCursor>>()
            + std::mem::size_of::<usize>()
    }

    /// Charge finite pointer/run-slot cleanup when ending the fixed slot, rather than hiding actual cursor Drop in the next step.
    pub(super) fn completed_drop_control() -> usize {
        std::mem::size_of::<Self>().div_ceil(std::mem::size_of::<usize>()) + PLAIN_CONTROL
    }

    /// An unsupported result remains an actual syntax outcome; RootLeaf preserves a hole instead of converting it to default styles.
    pub(super) fn pending_reason(&self) -> Option<MarkdownOrdinaryPendingReason> {
        self.pending
    }

    /// Transfer only completed runs from the original output owner; the parent jointly admits each, and incomplete/unsupported work grants no output.
    pub(super) fn take_proven_run(
        &mut self,
        index: usize,
    ) -> Result<Option<MarkdownPresentationRun>, MarkdownTableRequestError> {
        if !self.done()
            || self.pending.is_some()
            || self.run_next != self.requested.end
            || self.len == 0
        {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        Ok(if let Some(code) = self.code.as_mut() {
            code.output.get_mut(index).and_then(Option::take)
        } else if let Some(bounded) = self.bounded.as_mut() {
            bounded.output.get_mut(index).and_then(Option::take)
        } else {
            self.runs.get_mut(index).and_then(Option::take)
        })
    }

    /// Transfer validated link destinations through the same publication path, moving their existing ownership without copying target bytes.
    pub(super) fn take_proven_link(
        &mut self,
    ) -> Result<Option<MarkdownSourceLink>, MarkdownTableRequestError> {
        if !self.done() || self.pending.is_some() {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        Ok(if let Some(code) = self.code.as_mut() {
            code.take_link()
        } else {
            self.bounded
                .as_mut()
                .and_then(|bounded| bounded.links.pop())
        })
    }

    /// Return the original source-wide matching request after this block completes so the next block reuses its completed context.
    pub(super) fn return_underline(&mut self) -> Option<MarkdownTableUnderlineRequest> {
        self.underline.take()
    }

    /// After Leaf moves output item by item, retain only a proved ordinary prefix; complex inline, container, or unfinished jobs cannot remain.
    /// This remains the parent's sole admitted semantic slot, without publication, source frames, or historical runs.
    pub(super) fn retains_plain_prefix(&self) -> bool {
        self.done()
            && self.pending.is_none()
            && self.cell.is_none()
            && self.line.is_none()
            && self.cursor.is_some()
            && self.proof.is_some()
            && self.projection.is_some()
            && self.projected.is_some()
            && self.bounded.is_none()
            && self.code.is_none()
            && self.underline.is_none()
            && self.runs.iter().all(Option::is_none)
    }

    /// The current record must retain the same actual seed; equal text or coordinates cannot authorize continuing another block.
    pub(super) fn retains_plain_block(&self, block: &Rc<OrdinaryBlockRecord>) -> bool {
        self.retains_plain_prefix()
            && self
                .block
                .as_ref()
                .is_some_and(|current| Rc::ptr_eq(current, block))
    }

    /// Lend only the actually read plain prefix of an original Root Paragraph for selection-invariance checks, not the whole block.
    /// Derive its UTF-16 end from this Bounds/Coordinates projection without scanning distant headings or an unknown suffix.
    #[cfg(feature = "paged-line-compose")]
    pub(super) fn selection_invariant_prefix(
        &self,
        source: &MarkdownTableSourceIdentity,
        leaf: &VerifiedMarkdownRootLeafViewport,
    ) -> Option<PlainPrefixEnd> {
        if !self.retains_plain_prefix() {
            return None;
        }
        let block = self.block.as_ref()?;
        let proof = self.proof.as_ref()?;
        let coverage = self.byte_coverage.as_ref()?;
        let projected = self.projected?;
        // The same seed binds original Parser options and Root scope. Headings, containers, cells, or marker-bearing proofs
        // grant no such capability; starting at source zero excludes external tags/containers that could affect the prefix's styles.
        if block.seed.scope_depth() != 1
            || block.seed.kind() != InlineBlockKind::Paragraph
            || block.seed.physical_range().start != 0
            || block.seed.source_range().start != 0
            || proof.kind() != InlineBlockKind::Paragraph
            || proof.marker_range().is_some()
            || proof.physical_range().start != 0
            || proof.source_range().start != 0
            || proof.source_revision() != source.generation()
            || proof.source_end() != source.byte_len()
            || coverage.end > proof.proven_end()
            || projected[3] < self.requested.end
            || !leaf.covers_plain_block(block, &self.requested)
        {
            return None;
        }
        // Whole-scalar byte coverage may slightly exceed the UTF-16 query. Grant only the projected requested.end,
        // never a paragraph length guessed from source_range.end/proven_end, and grant no grapheme-boundary authority.
        Some(PlainPrefixEnd {
            byte: coverage.end,
            utf16: self.requested.end,
        })
    }

    /// Continue this ordinary block only after the parent measures the new window/complete selection and returns the sole underline owner.
    /// Unchanged selection allows nonoverlapping forward movement; a new selection may reproject the same seed's proved prefix with fresh coordinates.
    /// Another block or complex path uses cold preparation, and unknown suffixes inherit no default-style proof.
    pub(super) fn continue_closed_block(
        &mut self,
        requested: Range<usize>,
        selection: MarkdownTextSelection,
        byte_coverage: Range<usize>,
        selection_bytes: Range<usize>,
        underline: &mut Option<MarkdownTableUnderlineRequest>,
        quota: MarkdownTableWindowQuota,
    ) -> bool {
        let new_selection = self.selection != selection;
        if !self.retains_plain_prefix() || requested.is_empty() || byte_coverage.is_empty() {
            return false;
        }
        if !new_selection
            && (self.selection_bytes != selection_bytes
                || requested.start < self.requested.end
                || self
                    .byte_coverage
                    .as_ref()
                    .is_none_or(|previous| byte_coverage.start < previous.end))
        {
            return false;
        }
        // The sole Leaf caller just checked actual SourceRecord Rc identity. Check this slot's original seed envelope next;
        // no callback, source read, or suspension separates these two Core checks.
        let physical = self.block.as_ref().unwrap().seed.physical_range();
        if physical.start > byte_coverage.start || byte_coverage.end > physical.end {
            return false;
        }
        // New selection comes only from accepted parent reprojection. Leaf already relocated all four endpoints on the same Source
        // and checked the complete current selection; old runs do not participate. Cursor/proof describes this opaque seed's original
        // byte prefix only, without selection visibility or publication authority.
        if new_selection {
            self.selection = selection;
            self.selection_bytes = selection_bytes;
            self.projection = None;
            self.projected = None; // Reprove backward coordinates from original Source instead of rewinding the old forward cursor.
        }
        let covered = self
            .proof
            .as_ref()
            .is_some_and(|proof| byte_coverage.end <= proof.proven_end());
        // Replace the obsolete handle with this new batch's shared quota, without renewing cumulative scan fuel or inventing inventory.
        self.run_next = requested.start;
        self.requested = requested;
        self.byte_coverage = Some(byte_coverage);
        if !covered {
            self.proof = None;
        } // Beyond the scanned prefix, the same cursor must actually extend_to/read source; selection cannot enlarge proof.
        // Unchanged selection retains the completed coordinate cursor; new selection discarded old coordinates above.
        // projected distinguishes actual completion from preparation, never reusing old numeric values as a new window's result.
        self.underline = underline.take();
        self.runs = std::array::from_fn(|_| None);
        self.len = 0;
        self.underline_index = 0;
        self.saw_intersection = true;
        self.quota = quota;
        self.phase = SemanticPhase::AwaitSource;
        true
    }

    /// Only actual scoped Code prefixes require original source records; Root/cell and already verified owners do not use this branch.
    pub(super) fn prefix_origin_record_index(&self) -> Option<usize> {
        self.code.as_ref()?.prefix_origin_record_index()
    }

    /// Borrow one existing record per parent step; transfer physical-line facts only for the same Rc block and exact prefix.
    pub(super) fn advance_prefix_origin(
        &mut self,
        record: &Rc<SourceRecord>,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let code = self
            .code
            .as_mut()
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        let line = match record.as_ref() {
            SourceRecord::Line(line)
                if line.semantic_block.as_ref().is_some_and(|block| {
                    self.block
                        .as_ref()
                        .is_some_and(|current| Rc::ptr_eq(block, current))
                }) =>
            {
                Some(&line.origin)
            }
            _ => None,
        };
        code.advance_prefix_origin(line, &self.selection_bytes)
    }

    pub(super) fn preparing(&self) -> bool {
        matches!(self.phase, SemanticPhase::Bounds)
    }
    pub(super) fn done(&self) -> bool {
        matches!(self.phase, SemanticPhase::Done)
    }

    /// Byte coverage exists only after actual location or transfer of proved parent endpoints; an empty selection is still a valid fact.
    pub(super) fn verified_selection_bytes(&self) -> Option<Range<usize>> {
        self.byte_coverage.as_ref()?;
        Some(self.selection_bytes.clone())
    }
    pub(super) fn publication_control(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<usize, MarkdownTableRequestError> {
        // Release Build's completed temporary pair directory within this publication; its finite element Drop cannot spill into the next quota.
        let cleanup = match self.underline.as_ref() {
            Some(underline) => {
                PLAIN_CONTROL
                    + underline
                        .ready_pairs_for(document)
                        .map_err(MarkdownTableRequestError::Window)?
                        .ok_or(MarkdownTableRequestError::InvalidRequest)?
                        .len()
            }
            None => 0,
        };
        // Create a Box only for completed finite semantics; Pending pays no nonexistent Box allocation/payload transfer.
        Ok(cleanup
            + if self.done() && self.pending.is_none() {
                PLAIN_CONTROL
            } else {
                0
            })
    }
    pub(super) fn publication_payload_metadata(&self) -> usize {
        // Reservation covers the worst branch; actual work/exported inventory charges only a Box payload that really exists.
        if self.done() && self.pending.is_none() {
            plain_publication_metadata()
        } else {
            0
        }
    }
    pub(super) fn needs_underline_workspace(&self) -> bool {
        // Code first acquires the shared owner for storage/reference work, so reserve workspace through the parent's sole entry.
        // Waiting for an existing Underline value would skip actual allocation admission for ordinary Source or the first cell.
        matches!(self.phase, SemanticPhase::Code | SemanticPhase::Underline)
            && self.underline.is_none()
    }

    pub(super) fn retarget(&mut self, requested: Range<usize>) -> SemanticWork {
        // Relocate only four finite request endpoints; keep original cursor/unsupported result/seed for comparison with actual new coverage.
        let ordered = ordered_selection(self.selection);
        self.positions = [
            PositionLocator::new(requested.start, false),
            PositionLocator::new(requested.end, true),
            PositionLocator::new(ordered.start, false),
            PositionLocator::new(ordered.end, true),
        ];
        self.requested = requested;
        self.position_index = 0;
        self.byte_coverage = None;
        self.proof = None;
        self.projection = None;
        self.projected = None;
        self.underline = None;
        self.bounded = None;
        self.code = None;
        self.runs = std::array::from_fn(|_| None);
        self.len = 0;
        self.run_next = self.requested.start;
        self.underline_index = 0;
        self.saw_intersection = false;
        self.phase = SemanticPhase::Bounds;
        SemanticWork {
            structural_transitions: PLAIN_CONTROL,
            metadata_bytes: std::mem::size_of::<[PositionLocator; 4]>()
                + std::mem::size_of::<[Option<MarkdownPresentationRun>; MAX_PLAIN_RUNS]>(),
            ..SemanticWork::default()
        }
    }

    pub(super) fn considers(&self, physical: &Range<usize>) -> bool {
        self.byte_coverage
            .as_ref()
            .is_some_and(|coverage| physical.start < coverage.end && coverage.start < physical.end)
    }

    pub(super) fn consider(&mut self, block: &Rc<OrdinaryBlockRecord>) {
        let Some(coverage) = &self.byte_coverage else {
            return;
        };
        let physical = block.seed.physical_range();
        self.saw_intersection |= physical.start < coverage.end && coverage.start < physical.end;
        if physical.start <= coverage.start
            && coverage.end <= physical.end
            && self
                .block
                .as_ref()
                .is_none_or(|current| !Rc::ptr_eq(current, block))
        {
            // Only another real Fact can replace the block; discard old progress rather than treating its range as a new seed.
            self.cursor = None;
            self.bounded = None;
            self.code = None;
            self.pending = None;
            self.block = Some(Rc::clone(block));
        }
    }

    pub(super) fn advance_bounds(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let work = self.positions[self.position_index].advance(document, budget)?;
        if self.positions[self.position_index].result.is_some() {
            self.position_index += 1;
            if self.position_index == self.positions.len() {
                // Obtain context with complete-scalar byte boundaries while public coverage remains clipped to the original UTF-16 request.
                let coverage = self.positions[0].result.unwrap()..self.positions[1].result.unwrap();
                self.selection_bytes =
                    self.positions[2].result.unwrap()..self.positions[3].result.unwrap();
                if coverage.is_empty() {
                    return Err(MarkdownTableRequestError::InvalidRequest);
                }
                if self.block.as_ref().is_some_and(|block| {
                    let range = block.seed.physical_range();
                    range.start > coverage.start || coverage.end > range.end
                }) {
                    // The new window left the old block; its prefix cannot continue as proof for another block.
                    self.block = None;
                    self.cursor = None;
                    self.pending = None;
                }
                self.byte_coverage = Some(coverage);
                self.phase = SemanticPhase::AwaitSource;
            }
        }
        Ok(work)
    }

    pub(super) fn advance_after_sources(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
        limits: (usize, usize),
        global: Option<&mut GlobalFrontier>,
    ) -> Result<(SemanticWork, usize), MarkdownTableRequestError> {
        let mut work = SemanticWork {
            structural_transitions: PLAIN_CONTROL,
            ..SemanticWork::default()
        };
        let mut new_runs = 0;
        match self.phase {
            SemanticPhase::AwaitSource => {
                if self.block.is_none() {
                    // Source facts may publish, but without one real InlineBlock covering the interval, do not certify default styles.
                    self.pending = Some(if self.saw_intersection {
                        MarkdownOrdinaryPendingReason::IncompleteCoverage
                    } else {
                        MarkdownOrdinaryPendingReason::MissingClosedBlock
                    });
                    self.phase = SemanticPhase::Done;
                } else if self.pending.is_some() {
                    // Forward movement cannot erase the same seed's original unsupported complex/non-Root outcome.
                    self.phase = SemanticPhase::Done;
                } else {
                    // Source and coverage are complete, but actual Root/LUT proof and global context are still required for readiness.
                    self.phase = if self.proof.is_some() {
                        // Retained plain-syntax proof already reaches this byte end; redo coordinates, underline, and selection visibility
                        // without borrowing a distant frame merely to return the same Proven result again.
                        SemanticPhase::Coordinates
                    } else if self.block.as_ref().is_some_and(|block| {
                        matches!(block.seed.kind(), InlineBlockKind::SetextHeading(_))
                            && BoundedInline::supports_scoped(block)
                    }) {
                        SemanticPhase::ScopedBounded
                    } else if self.line.is_some() {
                        SemanticPhase::Code
                    } else {
                        SemanticPhase::Proof
                    };
                }
            }
            SemanticPhase::ScopedBounded => {
                self.bounded = Some(BoundedInline::for_scoped(
                    self.block.as_ref().unwrap(),
                    limits,
                    &self.quota,
                )?);
                self.phase = SemanticPhase::BoundedInline;
                work.metadata_bytes = BoundedInline::scoped_metadata();
            }
            SemanticPhase::Proof => {
                let end = self.byte_coverage.as_ref().unwrap().end;
                match self.cursor.as_mut() {
                    None => {
                        // Construction retains original capture only; the next budgeted vendor step initializes LUT and scope.
                        let seed = &self.block.as_ref().unwrap().seed;
                        let limits = InlinePlainLimits {
                            max_byte_work: limits.0,
                            max_transitions: limits.1,
                        };
                        self.cursor = Some(
                            seed.plain_cursor(end, limits)
                                .map_err(MarkdownTableRequestError::Global)?,
                        );
                    }
                    Some(cursor) if cursor.requested_end() < end && self.proof.is_none() => {
                        // Extend only required_end after the original prefix completes, preserving cumulative limits and actual scan position.
                        let measured = cursor
                            .extend_to(end)
                            .map_err(MarkdownTableRequestError::Global)?;
                        work.structural_transitions += measured.transitions;
                    }
                    Some(cursor) => {
                        // The same original cursor scans only its required_end prefix; actual frame/tree reads belong to this request's account.
                        let tree = tree_height(document);
                        let child = child_budget(budget - PLAIN_CONTROL, tree)?;
                        let frame = document
                            .table_source_frame(cursor.next_byte(), child)
                            .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
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
                        // Consume the step only after successful frame return, so parser completion cannot mask a mapping failure.
                        frame
                            .finish()
                            .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
                        let step = step?;
                        let measured = match step {
                            InlinePlainStep::Progress { work }
                            | InlinePlainStep::NeedInput { work, .. } => work, // A frame end continues reading, without claiming a complete block or EOF.
                            InlinePlainStep::NeedsGeneralInline { work } => {
                                if self
                                    .cursor
                                    .as_ref()
                                    .unwrap()
                                    .bounded_inline_range()
                                    .is_some()
                                {
                                    self.phase = SemanticPhase::BoundedInline;
                                } else {
                                    self.phase = SemanticPhase::Code;
                                }
                                work
                            }
                            InlinePlainStep::NonRootScope { work } => {
                                // Only complete short blocks enter original scoped context; long blocks still need actual Origin rather than pretending Root scope.
                                if self
                                    .block
                                    .as_ref()
                                    .is_some_and(|block| BoundedInline::supports_scoped(block))
                                {
                                    self.phase = SemanticPhase::ScopedBounded;
                                } else {
                                    self.pending =
                                        Some(MarkdownOrdinaryPendingReason::NonRootScope);
                                    self.phase = SemanticPhase::Done;
                                }
                                work
                            }
                            InlinePlainStep::Proven { proof, work } => {
                                // This proves a Root plain prefix only; UTF-16 boundaries and underline context across omitted text still need completion.
                                self.proof = Some(proof);
                                self.phase = SemanticPhase::Coordinates;
                                work
                            }
                        };
                        work.source_bytes = measured.byte_reads;
                        work.source_tree_steps = tree;
                        work.structural_transitions += measured.transitions + tree;
                    }
                }
            }
            SemanticPhase::BoundedInline => {
                if let Some(bounded) = self.bounded.as_mut() {
                    if bounded.references_attached()
                        && self.underline.as_ref().is_some_and(|underline| {
                            underline
                                .ready_pairs_for(document)
                                .is_ok_and(|pairs| pairs.is_none())
                        })
                    {
                        let measured = self.underline.as_mut().unwrap().advance(
                            document,
                            global.ok_or(MarkdownTableRequestError::InvalidRequest)?,
                            budget - PLAIN_CONTROL,
                        )?;
                        work = underline_work(measured);
                        work.structural_transitions += PLAIN_CONTROL;
                    } else {
                        let (measured, complete) = bounded.advance(
                            document,
                            &mut self.underline,
                            &self.quota,
                            self.selection,
                            &self.selection_bytes,
                            budget - PLAIN_CONTROL,
                        )?;
                        work = measured;
                        work.structural_transitions += PLAIN_CONTROL;
                        if complete {
                            if bounded.unsupported_scope() {
                                self.pending = Some(MarkdownOrdinaryPendingReason::NonRootScope);
                                self.phase = SemanticPhase::Done;
                            } else {
                                self.phase = SemanticPhase::Build;
                            }
                        }
                    }
                } else {
                    self.bounded = BoundedInline::new(
                        self.cursor.as_ref().unwrap(),
                        self.block.as_ref().unwrap(),
                        &self.quota,
                    )?;
                    work.metadata_bytes = std::mem::size_of::<BoundedInline>();
                }
            }
            SemanticPhase::Code => {
                if self.underline.is_none() {
                    self.underline = Some(if let Some(cell) = &self.cell {
                        document.table_cell_underline_request(cell, self.quota.clone())?
                    } else {
                        document
                            .table_underline_request(
                                std::slice::from_ref(self.byte_coverage.as_ref().unwrap()),
                                self.quota.clone(),
                            )
                            .map_err(MarkdownTableRequestError::Window)?
                    });
                    return Ok((work, 0)); // Construct shared actual resources first; a later step drives the original Code cursor.
                }
                if let Some(code) = self.code.as_mut() {
                    if code.preparing() {
                        work = code.prepare_cursor(
                            self.block.as_ref().unwrap(),
                            self.line.as_ref(),
                            limits,
                            &self.quota,
                            budget - PLAIN_CONTROL,
                        )?;
                        work.structural_transitions += PLAIN_CONTROL;
                        return Ok((work, 0)); // Yield after actual cursor allocation, without reading Source again in the same step.
                    }
                    let (measured, complete) = code.advance(
                        document,
                        &self.selection_bytes,
                        &self.quota,
                        budget - PLAIN_CONTROL,
                        self.underline.as_mut().unwrap(),
                        global,
                    )?;
                    work = measured;
                    work.structural_transitions += PLAIN_CONTROL;
                    if complete {
                        if code.general {
                            if self
                                .block
                                .as_ref()
                                .is_some_and(|block| BoundedInline::supports_scoped(block))
                            {
                                // Successful streaming Plain/Code/Emphasis paths avoid body copies. Only an actual general-inline outcome
                                // falls through to a complete short-block Parser; return old scratch before owning another inline/matching job.
                                code.release_scratch();
                                self.code = None;
                                self.cursor = None;
                                self.phase = SemanticPhase::ScopedBounded;
                            } else {
                                self.pending =
                                    Some(MarkdownOrdinaryPendingReason::NeedsGeneralInline);
                                self.phase = SemanticPhase::Done;
                            }
                        } else {
                            self.phase = SemanticPhase::Underline;
                        }
                    }
                } else {
                    self.code = CodeJob::new(
                        self.block.as_ref().unwrap(),
                        self.byte_coverage.as_ref().unwrap().clone(),
                        self.requested.clone(),
                        &self.quota,
                        budget - PLAIN_CONTROL,
                    )?;
                    if self.code.is_none() {
                        self.pending = Some(MarkdownOrdinaryPendingReason::NeedsGeneralInline);
                        self.phase = SemanticPhase::Done;
                    }
                    work.metadata_bytes =
                        usize::from(self.code.is_some()) * std::mem::size_of::<CodeJob>();
                }
            }
            SemanticPhase::Coordinates => {
                if self.projected.is_none()
                    && let Some(projection) = self.projection.as_mut()
                {
                    // Advance the original coordinate cursor for an unfinished group; this branch's borrow proves the cursor exists.
                    let (measured, values) =
                        projection.advance(document, budget - PLAIN_CONTROL)?;
                    work = measured;
                    work.structural_transitions += PLAIN_CONTROL;
                    if let Some(values) = values {
                        // Convert style segments to absolute UTF-16 only after decoding all visible boundaries on the original source.
                        self.projected = Some(values);
                        // The completed cursor holds four boundaries/metrics, not text; same-block Leaf continuation reuses this instance.
                        self.phase = SemanticPhase::Underline;
                    }
                } else {
                    let coverage = self.byte_coverage.as_ref().unwrap();
                    let proof = self.proof.as_ref().unwrap();
                    let owner = proof
                        .source_range()
                        .start
                        .clamp(coverage.start, coverage.end);
                    let marker = proof.marker_range().map_or(coverage.end, |range| {
                        range.start.clamp(coverage.start, coverage.end)
                    });
                    // Convert actual visible boundaries only, without repeatedly decoding a giant paragraph merely to locate a distant owner.
                    let boundaries = [coverage.start, owner, marker, coverage.end];
                    let measured = if let Some(projection) = self.projection.as_mut() {
                        // Same-block forward movement resumes from the actual previous end; later advance still reads intervening bytes.
                        // Small style windows must not repeatedly restart prefix decoding from the same source subpiece.
                        projection.continue_at(boundaries)?
                    } else {
                        let (projection, measured) =
                            Coordinates::new(document, boundaries, budget - PLAIN_CONTROL)?;
                        self.projection = Some(projection);
                        measured
                    };
                    self.projected = None;
                    work = measured;
                    work.structural_transitions += PLAIN_CONTROL;
                }
            }
            SemanticPhase::Underline => {
                match self.underline.as_mut() {
                    None => {
                        // The parent jointly admitted the sole underline workspace before this phase; attach only actual finite byte coverage.
                        self.underline = Some(if let Some(cell) = &self.cell {
                            document.table_cell_underline_request(cell, self.quota.clone())?
                        } else {
                            document
                                .table_underline_request(
                                    std::slice::from_ref(self.byte_coverage.as_ref().unwrap()),
                                    self.quota.clone(),
                                )
                                .map_err(MarkdownTableRequestError::Window)?
                        });
                    }
                    Some(underline)
                        if underline
                            .ready_pairs_for(document)
                            .map_err(MarkdownTableRequestError::Window)?
                            .is_some() =>
                    {
                        // Build queryable finite styles only after original global context completes and delivers actual pairs.
                        self.phase = SemanticPhase::Build;
                    }
                    Some(underline) => {
                        // A source byte may also require UTF-8 prefix validation; charge all actual reads/tree paths to the original account.
                        let measured = underline.advance(
                            document,
                            global.ok_or(MarkdownTableRequestError::InvalidRequest)?,
                            budget - PLAIN_CONTROL,
                        )?;
                        work.source_bytes = measured.source_bytes + measured.global_source_bytes;
                        work.utf8_validation_bytes = measured.utf8_validation_bytes;
                        work.source_tree_steps =
                            measured.source_tree_steps + measured.global_source_tree_steps;
                        work.structural_transitions += measured.structural_transitions;
                        work.copied_source_bytes = measured.copied_source_bytes;
                        work.metadata_bytes = measured.metadata_bytes;
                        work.normalizer_input_bytes = measured.normalizer_input_bytes;
                        work.parser_input_bytes = measured.parser_input_bytes;
                        work.allocated_source_capacity = measured.allocated_source_capacity;
                    }
                }
            }
            SemanticPhase::Build => {
                if self.run_next == self.requested.end {
                    if let Some(underline) = self.underline.as_mut() {
                        underline.close_inline_stack()?;
                    }
                    // Complete only after every queryable position has an actual run; positions outside coverage remain None, not default-styled.
                    self.phase = SemanticPhase::Done;
                    if let Some(bounded) = self.bounded.as_mut() {
                        bounded.release_scratch();
                    }
                    if let Some(code) = self.code.as_mut() {
                        code.release_scratch();
                    }
                } else {
                    let pairs = self
                        .underline
                        .as_ref()
                        .unwrap()
                        .ready_pairs_for(document)
                        .map_err(MarkdownTableRequestError::Window)?
                        .ok_or(MarkdownTableRequestError::InvalidRequest)?;
                    if pairs
                        .get(self.underline_index)
                        .is_some_and(|pair| pair.close.end <= self.run_next)
                    {
                        // A completed pair wholly before this position no longer affects remaining finite coverage.
                        self.underline_index += 1;
                    } else {
                        let mut end = self.requested.end;
                        let mut styles = MarkdownPresentationStyles::default();
                        if let Some(code) = self.code.as_mut() {
                            let (measured, query) =
                                code.advance_query(self.run_next, budget - PLAIN_CONTROL);
                            work.structural_transitions += measured.structural_transitions;
                            let Some(query) = query else {
                                return Ok((work, 0)); // Do not commit half a run while style intersections are incomplete; resume this same query.
                            };
                            end = end.min(query.utf16_range.end);
                            styles = query.styles;
                        } else if let Some(bounded) = self.bounded.as_ref() {
                            let query = bounded.query(self.run_next, self.requested.end);
                            end = end.min(query.utf16_range.end);
                            styles = query.styles;
                        } else {
                            let values = self.projected.unwrap();
                            let proof = self.proof.as_ref().unwrap();
                            let heading = match proof.kind() {
                                InlineBlockKind::Paragraph | InlineBlockKind::TableCell => {
                                    MarkdownPresentationStyles::default()
                                } // Original scope projection supplies container styles; cell content cannot determine them.
                                InlineBlockKind::SetextHeading(
                                    pulldown_cmark::HeadingLevel::H1,
                                ) => MarkdownPresentationStyles::HEADING_1,
                                InlineBlockKind::SetextHeading(_) => {
                                    MarkdownPresentationStyles::HEADING_2
                                }
                            };
                            if self.run_next >= values[1] {
                                // Heading style starts at the original Parser owner; physical indentation before it retains default style.
                                styles = styles.union(heading);
                            } else {
                                end = end.min(values[1]);
                            }
                            if proof.marker_range().is_some() {
                                // Setext markers use only proved actual ranges; complete owner and complete selection determine visibility.
                                if self.run_next >= values[2] {
                                    styles = styles.union(MarkdownPresentationStyles::MARKER);
                                    if !markdown_selection_intersects(
                                        &self.selection_bytes,
                                        &proof.source_range(),
                                    ) {
                                        styles =
                                            styles.union(MarkdownPresentationStyles::HIDDEN_MARKER);
                                    }
                                } else {
                                    end = end.min(values[2]);
                                }
                            }
                        }
                        if let Some(pair) = pairs.get(self.underline_index) {
                            // The source-wide scanner supplies actual pairs; clip only their boundaries rather than reparsing omitted text as local tags.
                            for boundary in [
                                pair.open.start,
                                pair.open.end,
                                pair.content.start,
                                pair.content.end,
                                pair.close.start,
                                pair.close.end,
                            ] {
                                if boundary > self.run_next {
                                    end = end.min(boundary);
                                }
                            }
                            if pair.content.contains(&self.run_next)
                                && !styles.contains(MarkdownPresentationStyles::INLINE_CODE)
                                && !styles.contains(MarkdownPresentationStyles::FENCED_CODE)
                            {
                                // An external pair may span code in the original projection, but underline does not enter proved code or its delimiters.
                                styles = styles.union(MarkdownPresentationStyles::UNDERLINE);
                            }
                            if pair.open.contains(&self.run_next)
                                || pair.close.contains(&self.run_next)
                            {
                                styles = styles.union(MarkdownPresentationStyles::MARKER);
                                if !markdown_selection_intersects(
                                    &ordered_selection(self.selection),
                                    &(pair.open.start..pair.close.end),
                                ) {
                                    styles =
                                        styles.union(MarkdownPresentationStyles::HIDDEN_MARKER);
                                }
                            }
                        }
                        if end <= self.run_next {
                            // Reject inconsistent coordinates instead of skipping characters and leaving implicit holes in query coverage.
                            return Err(MarkdownTableRequestError::InvalidRequest);
                        }
                        if let Some(code) = self.code.as_mut() {
                            if let Some(previous) = code.output.last_mut().and_then(Option::as_mut)
                                && previous.styles == styles
                            {
                                previous.utf16_range.end = end;
                            } else {
                                code.push_output(
                                    MarkdownPresentationRun {
                                        utf16_range: self.run_next..end,
                                        styles,
                                    },
                                    &self.quota,
                                )?;
                                self.len += 1;
                                new_runs = 1;
                            }
                        } else if let Some(bounded) = self.bounded.as_mut() {
                            if let Some(previous) =
                                bounded.output.last_mut().and_then(Option::as_mut)
                                && previous.styles == styles
                            {
                                previous.utf16_range.end = end;
                            } else {
                                bounded.push_output(
                                    MarkdownPresentationRun {
                                        utf16_range: self.run_next..end,
                                        styles,
                                    },
                                    &self.quota,
                                )?;
                                self.len += 1;
                                new_runs = 1;
                            }
                        } else if self.len > 0
                            && self.runs[self.len - 1].as_ref().unwrap().styles == styles
                        {
                            // Merge adjacent equal styles in the fixed base slots without a boundary Vec, sort, or paragraph-sized output queue.
                            self.runs[self.len - 1].as_mut().unwrap().utf16_range.end = end;
                        } else {
                            if self.len == MAX_PLAIN_RUNS {
                                // A full finite base directory fails the item; do not retain partial Ready or fall back to unproved default styles.
                                return Err(MarkdownTableRequestError::Window(
                                    MarkdownTableWindowError::SemanticLimit,
                                ));
                            }
                            self.runs[self.len] = Some(MarkdownPresentationRun {
                                utf16_range: self.run_next..end,
                                styles,
                            });
                            self.len += 1;
                            new_runs = 1;
                        }
                        self.run_next = end;
                        work.metadata_bytes = std::mem::size_of::<MarkdownPresentationRun>();
                    }
                }
            }
            SemanticPhase::Bounds => return Err(MarkdownTableRequestError::InvalidRequest), // The parent must advance endpoint location first.
            SemanticPhase::AtxBounded => return Err(MarkdownTableRequestError::InvalidRequest), // ATX requires the parent's current real leaf, not authority invented from an empty block slot.
            SemanticPhase::Done => (), // The parent performs one explicit publication; do not allocate another result here.
        }
        Ok((work, new_runs))
    }

    pub(super) fn finish(
        &mut self,
        source: MarkdownTableSourceIdentity,
        context_revision: u64,
        publication: Weak<SourcePublication>,
    ) -> Result<MarkdownOrdinarySemanticState, MarkdownTableRequestError> {
        if !self.done() {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        if let Some(underline) = self.underline.as_mut() {
            underline.close_inline_stack()?;
        }
        if let Some(reason) = self.pending {
            return Ok(MarkdownOrdinarySemanticState::Pending(reason));
        }
        if self.run_next != self.requested.end || self.len == 0 {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        // Original quota remains valid and publication prepaid actual cleanup; successors retain seed/cursor/fuel without the temporary pairs Vec.
        self.underline = None;
        self.projection = None;
        self.projected = None;
        let (rich_runs, links) = if let Some(code) = self.code.as_mut() {
            // The same Code job proves long-paragraph styles and complete click targets; ordinary publication transfers both.
            // Original quota already admitted these Vecs and immutable URIs, so take moves handles without copying targets or reissuing metadata.
            (
                std::mem::take(&mut code.output),
                std::mem::take(&mut code.links),
            )
        } else {
            self.bounded.as_mut().map_or_else(
                || (Vec::new(), Vec::new()),
                |bounded| {
                    (
                        std::mem::take(&mut bounded.output),
                        std::mem::take(&mut bounded.links),
                    )
                },
            )
        };
        // Parent publication already charged Box payload, handles, and explicit publication work; transfer the fixed array without copying text.
        Ok(MarkdownOrdinarySemanticState::PlainReady(Box::new(
            VerifiedMarkdownPlainViewport {
                source,
                context_revision,
                selection: self.selection,
                publication,
                requested: self.requested.clone(),
                runs: std::mem::replace(&mut self.runs, std::array::from_fn(|_| None)),
                len: self.len,
                block: Rc::clone(self.block.as_ref().unwrap()),
                rich_runs,
                links,
            },
        )))
    }
}

/// Return the sole Global lookahead's full work, including finite reference copies/Parser input rather than hiding them in underline byte progress.
fn underline_work(
    measured: crate::markdown_document_buffer::table_source::MarkdownTableUnderlineRequestStep,
) -> SemanticWork {
    SemanticWork {
        source_bytes: measured.source_bytes + measured.global_source_bytes,
        utf8_validation_bytes: measured.utf8_validation_bytes,
        source_tree_steps: measured.source_tree_steps + measured.global_source_tree_steps,
        structural_transitions: measured.structural_transitions,
        copied_source_bytes: measured.copied_source_bytes,
        metadata_bytes: measured.metadata_bytes,
        normalizer_input_bytes: measured.normalizer_input_bytes,
        parser_input_bytes: measured.parser_input_bytes,
        allocated_source_capacity: measured.allocated_source_capacity,
    }
}

/// Decode a UTF-16 endpoint incrementally in its target leaf prefix, without reading text between two distant selection endpoints.
pub(super) struct PositionLocator {
    target: usize,
    ceil: bool,
    located: bool,
    next: usize,
    utf16: usize,
    scalar_start: usize,
    scalar_units: usize,
    remaining: u8,
    result: Option<usize>,
    leaf: Option<PositionLeaf>,
}

#[derive(Eq, PartialEq)]
/// Actual subpiece identity from original tree location, retaining scalars without strongly owning a document, reader, tree node, or mapping.
struct PositionLeaf {
    node: usize,
    ordinal: Option<u8>,
    bytes: Range<usize>,
    utf16: Range<usize>,
    source: MarkdownPieceSource,
    source_byte_start: usize,
}

impl PositionLocator {
    pub(super) const PREFIX_REUSE_CONTROL: usize = 32;
    pub(super) const PREFIX_REUSE_METADATA: usize = 4 * std::mem::size_of::<usize>();

    pub(super) fn new(target: usize, ceil: bool) -> Self {
        Self {
            target,
            ceil,
            located: false,
            next: 0,
            utf16: 0,
            scalar_start: 0,
            scalar_units: 0,
            remaining: 0,
            result: None,
            leaf: None,
        }
    }

    /// An in-flight sole row query cannot switch points; comparing its existing target grants no source authority.
    pub(super) fn targets(&self, position: usize) -> bool {
        self.target == position
    }

    /// The parent checked same Source/complete selection; reuse only the completed scalar frontier within that same subpiece.
    /// Surrogate-interior floor results differ from next, so their consumed scalar tail cannot become a new starting point.
    pub(super) fn retarget_forward(&mut self, target: usize, ceil: bool) -> SemanticWork {
        let reusable = self.located
            && self.remaining == 0
            && self.result == Some(self.next)
            && target >= self.utf16
            && self
                .leaf
                .as_ref()
                .is_some_and(|leaf| target <= leaf.utf16.end && self.next <= leaf.bytes.end);
        let metadata_bytes = if reusable {
            // Retain actual leaf identity, decode position, and complete scalar state; discard the answer rather than reusing old byte output.
            self.target = target;
            self.ceil = ceil;
            self.result = None;
            std::mem::size_of::<usize>()
                + std::mem::size_of::<bool>()
                + std::mem::size_of::<Option<usize>>()
        } else {
            // Cross-piece, backward, incomplete, or surrogate-interior cases use original cold location without another locator/prefix cache.
            *self = Self::new(target, ceil);
            std::mem::size_of::<Self>()
        };
        SemanticWork {
            structural_transitions: Self::PREFIX_REUSE_CONTROL,
            metadata_bytes,
            ..SemanticWork::default()
        }
    }

    /// Row-start lookup needs one complete scalar, not the document's cold 4096-unit NSString cache or a full following window.
    /// Charge original location separately, then borrow at most four scalar bytes, allowing real piece boundaries to split them.
    pub(super) fn advance_row_source_viewport(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> (
        SemanticWork,
        Result<Option<Range<usize>>, MarkdownTableRequestError>,
    ) {
        if self.result.is_none() {
            // The same location implementation reports actual work even when Source/decoding fails after reading a prefix.
            let mut work = SemanticWork::default();
            let result = self
                .advance_position(document, budget, &mut work)
                .map(|()| None);
            return (work, result);
        }
        let mut work = SemanticWork {
            structural_transitions: PLAIN_CONTROL,
            ..SemanticWork::default()
        };
        let result = (|| {
            // Viewport endpoints allow floor/ceil, but a real row start must be a scalar boundary rather than the middle of a surrogate pair.
            if self.utf16 != self.target || self.remaining != 0 {
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            let start = self.result.expect("本分支已经完成原定位");
            let tree = tree_height(document);
            // Reserve up to four actual piece borrows and four-byte decoding, not an entire line scan or another Parser.
            let reservation = PLAIN_CONTROL + 4 * (tree + PLAIN_CONTROL + 4);
            if reservation > budget {
                return Err(MarkdownTableRequestError::WorkLimit);
            }
            let mut bytes = [0_u8; 4];
            let mut read = 0;
            let mut width = 1;
            while read < width {
                let frame = document
                    .table_source_frame(start + read, width - read)
                    .map_err(MarkdownTableRequestError::Source)?;
                work.source_tree_steps += tree;
                work.structural_transitions += tree + PLAIN_CONTROL;
                let count = frame.bytes.len();
                bytes[read..read + count].copy_from_slice(&frame.bytes);
                read += count;
                work.source_bytes += count;
                work.utf8_validation_bytes += count;
                work.structural_transitions += count * 2;
                frame.finish().map_err(MarkdownTableRequestError::Source)?;
                if count == 0 {
                    return Err(MarkdownTableRequestError::InvalidRequest);
                }
                if read == 1 {
                    width = match bytes[0] {
                        0x00..=0x7f => 1,
                        0xc2..=0xdf => 2,
                        0xe0..=0xef => 3,
                        0xf0..=0xf4 => 4,
                        _ => return Err(MarkdownTableRequestError::InvalidRequest),
                    };
                }
            }
            let scalar = std::str::from_utf8(&bytes[..read])
                .map_err(|_| MarkdownTableRequestError::InvalidRequest)?
                .chars()
                .next()
                .ok_or(MarkdownTableRequestError::InvalidRequest)?;
            Ok(Some(self.target..self.target + scalar.len_utf16()))
        })();
        (work, result) // Source-return or UTF-8 failure still reports actual borrowed bytes rather than zero work.
    }

    /// Let a same-source Leaf job reuse this original endpoint continuation, without a whole-paragraph UTF-16/text copy.
    pub(super) fn result(&self) -> Option<usize> {
        self.result
    }

    /// Attempt subpiece reuse only after this target's own tree/ordinal location, never from a guessed unlocated source piece.
    pub(super) fn needs_location(&self) -> bool {
        !self.located
    }

    /// Transfer completed scalar progress forward within the same actual subpiece only; the target retains its own position and floor/ceil policy.
    pub(super) fn reuse_completed_prefix(&mut self, previous: &Self) -> bool {
        let Some(leaf) = self.leaf.as_ref() else {
            return false; // Unlocated or larger-than-4-KiB leaves keep original independent decoding instead of scanning the whole paragraph.
        };
        if previous.leaf.as_ref() != Some(leaf)
            || !self.located
            || self.result.is_some()
            || self.remaining != 0
            || self.next != leaf.bytes.start
            || self.utf16 != leaf.utf16.start
            || previous.remaining != 0
            || previous.result != Some(previous.next)
            || previous.utf16 > self.target
            || previous.next <= self.next
        {
            // Different identity, started targets, partial scalars/floor results, or backward positions grant no continuation rights.
            return false;
        }
        self.next = previous.next;
        self.utf16 = previous.utf16;
        self.scalar_start = previous.scalar_start;
        self.scalar_units = previous.scalar_units;
        true // Move only four actual coordinate scalars, not results, candidate authority, read windows, or Source owners.
    }

    /// Reserve this state's actual minimum work before the parent combines Bounds steps; insufficient remainder waits for its next step.
    pub(super) fn minimum_budget(&self, document: &MarkdownDocumentBuffer) -> usize {
        if !self.located {
            // AVL location also fills fixed locator fields, so metadata work consumes the parent's step remainder too.
            PLAIN_CONTROL + tree_height(document) + std::mem::size_of::<Self>()
        } else if self.remaining == 0 && self.utf16 == self.target {
            PLAIN_CONTROL // At a complete scalar boundary, publish the actual byte position without a read window.
        } else {
            // Original advance borrows remaining/4 bytes; at least one byte is needed for decode progress, with tree work included in control.
            PLAIN_CONTROL + tree_height(document) + 4
        }
    }

    pub(super) fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let mut work = SemanticWork::default();
        self.advance_position(document, budget, &mut work)?;
        Ok(work)
    }

    /// Ordinary Bounds and row recovery share the same actual location step; row recovery reports already performed reads even on failure.
    fn advance_position(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
        work: &mut SemanticWork,
    ) -> Result<(), MarkdownTableRequestError> {
        work.structural_transitions = PLAIN_CONTROL;
        if !self.located {
            let tree = tree_height(document);
            if tree + PLAIN_CONTROL > budget {
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            let mut node = document
                .root
                .as_deref()
                .ok_or(MarkdownTableRequestError::InvalidRequest)?;
            let mut local = self.target;
            let mut bytes = 0;
            let mut utf16 = 0;
            // Complete subtrees use validated cached metrics; decode the actual leaf prefix in later steps instead of a synchronous 64-KiB scan.
            while let MarkdownPieceTreeNode::Branch { left, right, .. } = node {
                let metrics = left.metrics();
                if local < metrics.utf16_len {
                    node = left;
                } else {
                    local -= metrics.utf16_len;
                    bytes += metrics.byte_len;
                    utf16 += metrics.utf16_len;
                    node = right;
                }
            }
            if local > node.metrics().utf16_len {
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            let (piece, ordinal) = match node {
                MarkdownPieceTreeNode::OriginalSpan(span) => {
                    // Each target locates its actual ordinal/logical prefix independently; numeric proximity cannot borrow another piece's progress.
                    let hit = span
                        .locate_utf16(local, true)
                        .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
                    bytes += hit.prefix.byte_len;
                    utf16 += hit.prefix.utf16_len;
                    (hit.piece, Some(hit.ordinal))
                }
                MarkdownPieceTreeNode::Leaf(piece) => (piece.clone(), None),
                MarkdownPieceTreeNode::Branch { .. } => unreachable!("原AVL定位已经到达实际叶"),
            };
            self.leaf =
                (piece.byte_len <= crate::original_backing::ORIGINAL_READ_BYTES).then(|| {
                    PositionLeaf {
                        node: std::ptr::from_ref(node).addr(),
                        ordinal,
                        bytes: bytes..bytes + piece.byte_len,
                        utf16: utf16..utf16 + piece.metrics.utf16_len,
                        source: piece.source,
                        source_byte_start: piece.byte_start,
                    }
                });
            self.next = bytes;
            self.utf16 = utf16;
            self.located = true;
            work.source_tree_steps = tree;
            work.structural_transitions += tree;
            work.metadata_bytes = std::mem::size_of::<Self>();
            return Ok(()); // Report completed AVL location now, so cancellation cannot omit synchronous work.
        }
        if self.remaining == 0 && self.utf16 == self.target {
            self.result = Some(self.next);
            return Ok(());
        }
        let tree = tree_height(document);
        let wanted = budget
            .checked_sub(PLAIN_CONTROL + tree)
            .ok_or(MarkdownTableRequestError::InvalidRequest)?
            / 4;
        if wanted == 0 {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        let frame = document
            .table_source_frame(self.next, wanted)
            .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
        work.source_tree_steps = tree;
        work.structural_transitions += tree;
        for &byte in frame.bytes.iter() {
            work.source_bytes += 1;
            work.utf8_validation_bytes += 1;
            work.structural_transitions += 2;
            if self.remaining == 0 {
                self.scalar_start = self.next;
                let (width, units) = match byte {
                    0..=0x7f => (1, 1),
                    0xc2..=0xdf => (2, 1),
                    0xe0..=0xef => (3, 1),
                    0xf0..=0xf4 => (4, 2),
                    _ => return Err(MarkdownTableRequestError::InvalidRequest),
                };
                self.remaining = width - 1;
                self.scalar_units = units;
            } else {
                if byte & 0xc0 != 0x80 {
                    return Err(MarkdownTableRequestError::InvalidRequest);
                }
                self.remaining -= 1;
            }
            self.next += 1;
            if self.remaining == 0 {
                let end = self
                    .utf16
                    .checked_add(self.scalar_units)
                    .ok_or(MarkdownTableRequestError::WorkLimit)?;
                if self.target <= end {
                    // Expand surrogate-interior viewport endpoints to full scalars; public queries still clip to the original UTF-16 request.
                    self.result = Some(if self.ceil || self.target == end {
                        self.next
                    } else {
                        self.scalar_start
                    });
                    self.utf16 = end;
                    break;
                }
                self.utf16 = end;
            }
        }
        if frame.bytes.is_empty() && self.result.is_none() {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        frame
            .finish()
            .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
