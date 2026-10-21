//! Export finite ordinary-source facts from the same structural request; source completion alone grants no styles or hidden-marker authority.

use super::*;
use pulldown_cmark::global_frontier::{
    BlockOriginCursor, BlockOriginLimits, BlockOriginLine, BlockOriginStep, InlineBlockKind,
    LeafBlockCloseReason, LeafBlockDetails, LeafBlockKind, VerifiedGlobalInlineBlock,
    VerifiedGlobalLeafBlock,
};
use std::cell::Cell;
use std::rc::{Rc, Weak};

mod coordinates;
use coordinates::Coordinates;
mod required;
mod table_row;
pub use required::{
    MarkdownViewportSourceRequiredRowQuery, MarkdownViewportSourceRequiredRowResult,
};
pub use table_row::VerifiedMarkdownTableRowSource;
mod cell_inline;
pub use cell_inline::{MarkdownTableCellInlineStep, VerifiedMarkdownTableCellInline};
#[cfg(feature = "paged-line-compose")]
mod selection_prefix;
pub(in crate::markdown_document_buffer) mod semantic;
#[cfg(feature = "paged-line-compose")]
pub use selection_prefix::MarkdownSelectionStyleInvariant;
pub use semantic::{
    MarkdownOrdinaryPendingReason, MarkdownOrdinarySemanticState, VerifiedMarkdownPlainViewport,
};
use semantic::{OrdinaryBlockRecord, PlainSemanticJob};
mod leaf_semantic;
pub use leaf_semantic::{MarkdownRootLeafPendingReason, VerifiedMarkdownRootLeafViewport};
use leaf_semantic::{RootLeafSemanticJob, RootLeafSemanticPublication};

// Retain a finite page of line/leaf descriptors, not a text cache or directory of every paragraph line.
const MAX_SOURCE_RECORDS: usize = 256;
const CONTROL_COST: usize = 64;

pub(super) enum CapturedOrdinary {
    Inline(VerifiedGlobalInlineBlock),
    Leaf(VerifiedGlobalLeafBlock),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Current context supplied by the real owner on each call; an ordered range cannot replace directed anchor/head identity.
pub struct MarkdownViewportSourceAuthority {
    pub context_revision: u64,
    pub selection: MarkdownTextSelection,
    pub cancelled: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownViewportSourceStep {
    Progress { work: SemanticWork },
    SourcesAvailable { work: SemanticWork },
}

/// Physical lines come from the original BlockOriginCursor; UTF-16 coordinates are measured from that same source.
/// A line record grants no Markdown hiding, inline matching, or Native edit authority.
pub struct VerifiedMarkdownOrdinaryLine {
    block_kind: InlineBlockKind,
    block_lexical: Range<usize>,
    origin: BlockOriginLine,
    utf16: [usize; 4],
    /// A terminal line may also own a measured Setext tail; use that endpoint only for continuation while exposing the original physical Origin.
    semantic_utf16_end: usize,
    semantic_block: Option<Rc<OrdinaryBlockRecord>>,
}

impl VerifiedMarkdownOrdinaryLine {
    pub fn block_kind(&self) -> InlineBlockKind {
        self.semantic_block
            .as_ref()
            .map_or(self.block_kind, |block| block.seed.kind())
    }
    pub fn block_lexical_range(&self) -> &Range<usize> {
        &self.block_lexical
    }
    pub fn origin(&self) -> &BlockOriginLine {
        &self.origin
    }
    pub fn physical_utf16_range(&self) -> Range<usize> {
        self.utf16[0]..self.utf16[3]
    }
    pub fn prefix_utf16_range(&self) -> Range<usize> {
        self.utf16[0]..self.utf16[1]
    }
    pub fn content_utf16_range(&self) -> Range<usize> {
        self.utf16[1]..self.utf16[2]
    }
    pub fn eol_utf16_range(&self) -> Range<usize> {
        self.utf16[2]..self.utf16[3]
    }
}

/// Retain the original opaque LeafBlock; its enclosure is not content stripped of container prefixes.
/// Preserve consumed_end separately from source_range.end because the original producer gives them distinct meanings.
pub struct VerifiedMarkdownOrdinaryLeaf {
    block: VerifiedGlobalLeafBlock,
    physical_utf16: Range<usize>,
    #[cfg(feature = "paged-line-compose")]
    root_code_body: Option<RootCodeBody>,
}

/// Construct only after converting the original Root Code leaf's four endpoints, without accepting caller ranges or guessed body ends.
/// The closing-fence line and following block lie outside the half-open body; all four endpoints of both selections must remain inside it.
#[cfg(feature = "paged-line-compose")]
struct RootCodeBody {
    utf16: Range<usize>,
}

#[cfg(feature = "paged-line-compose")]
impl RootCodeBody {
    fn contains_selection(&self, selection: MarkdownTextSelection) -> bool {
        self.utf16.contains(&selection.anchor_utf16) && self.utf16.contains(&selection.head_utf16)
    }
}

/// Derive the body from this opaque leaf's actual Root scope and kind; visually similar code inside containers grants no such capability.
/// Borrow completed producer metadata only; Coordinates still measures actual source bytes into UTF-16.
fn root_code_body(block: &VerifiedGlobalLeafBlock) -> Option<&Range<usize>> {
    if block.scope_depth() != 0 {
        return None;
    }
    match (block.kind(), block.details()) {
        (LeafBlockKind::FencedCode, LeafBlockDetails::FencedCode { body_enclosure, .. })
        | (LeafBlockKind::IndentedCode, LeafBlockDetails::IndentedCode { body_enclosure, .. }) => {
            Some(body_enclosure)
        }
        _ => None,
    }
}

impl VerifiedMarkdownOrdinaryLeaf {
    pub fn kind(&self) -> LeafBlockKind {
        self.block.kind()
    }
    pub fn source_range(&self) -> Range<usize> {
        self.block.source_range()
    }
    pub fn physical_start(&self) -> usize {
        self.block.physical_start()
    }
    pub fn consumed_end(&self) -> usize {
        self.block.consumed_end()
    }
    pub fn close_reason(&self) -> LeafBlockCloseReason {
        self.block.close_reason()
    }
    pub fn details(&self) -> &LeafBlockDetails {
        self.block.details()
    }
    /// Measure only physical_start..consumed_end, without claiming the enclosure's additional trailing prefix.
    pub fn physical_utf16_range(&self) -> &Range<usize> {
        &self.physical_utf16
    }
}

enum SourceRecord {
    Line(VerifiedMarkdownOrdinaryLine),
    Leaf(VerifiedMarkdownOrdinaryLeaf),
}

impl SourceRecord {
    fn utf16_range(&self) -> Range<usize> {
        match self {
            Self::Line(line) => line.utf16[0]..line.semantic_utf16_end,
            Self::Leaf(leaf) => leaf.physical_utf16.clone(),
        }
    }
}

struct SourcePublication {
    source: MarkdownTableSourceIdentity,
    context_revision: u64,
    selection: MarkdownTextSelection,
    // Retire this publication only when the original request accepts selection reprojection. Retained old Rc handles
    // cannot regain style authority if anchor/head later return to the same values; structural records may still be shared.
    retired: Cell<bool>,
}

impl SourcePublication {
    /// Preserve source/cancellation/complete-selection checks before retirement; retiring this publication cannot mask the original source failure.
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
        self.require_live()
    }

    /// A weak semantic ticket checks original source/cancellation first, then this actual publication's retirement.
    fn require_live(&self) -> Result<(), MarkdownTableRequestError> {
        if self.retired.get() {
            return Err(MarkdownTableRequestError::PublicationSuperseded);
        }
        Ok(())
    }
}

/// Table results and finite ordinary facts produced by one request; this aggregate alone does not imply ordinary semantic readiness.
/// Retain immutable metadata, original shared scope pages, and weak source identity without owning document text.
pub struct VerifiedMarkdownViewportSources {
    table: VerifiedMarkdownTableViewport,
    records: Vec<Rc<SourceRecord>>,
    requested: Range<usize>,
    publication: Rc<SourcePublication>,
    retained_metadata: usize,
    blocks: Vec<Rc<OrdinaryBlockRecord>>,
    semantic: MarkdownOrdinarySemanticState,
}

impl VerifiedMarkdownViewportSources {
    /// Derived frozen-Source style boundaries check their own health but must still reject the original publication's retirement.
    /// This observes only the Rc lifetime state, not the full Source/selection/context contract of a live document.
    #[cfg(feature = "paged-line-compose")]
    pub(crate) fn require_live_publication(&self) -> Result<(), MarkdownTableRequestError> {
        self.publication.require_live()
    }

    /// SourcesOnly and Pending prove no default style; each Ready variant grants only its actual finite coverage.
    pub fn ordinary_semantics_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<&MarkdownOrdinarySemanticState, MarkdownTableRequestError> {
        self.check(document, authority)?;
        Ok(&self.semantic)
    }

    fn check(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<(), MarkdownTableRequestError> {
        self.publication.check(document, authority)
    }

    pub fn requested_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<&Range<usize>, MarkdownTableRequestError> {
        self.check(document, authority)?;
        Ok(&self.requested)
    }

    pub fn table_sources_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<&VerifiedMarkdownTableViewport, MarkdownTableRequestError> {
        self.check(document, authority)?;
        Ok(&self.table)
    }

    pub fn ordinary_lines_for<'a>(
        &'a self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<
        impl Iterator<Item = &'a VerifiedMarkdownOrdinaryLine> + use<'a>,
        MarkdownTableRequestError,
    > {
        self.check(document, authority)?;
        Ok(self
            .records
            .iter()
            .filter_map(|record| match record.as_ref() {
                SourceRecord::Line(line)
                    if line.physical_utf16_range().start < self.requested.end
                        && self.requested.start < line.physical_utf16_range().end =>
                {
                    Some(line)
                }
                SourceRecord::Line(_) => None, // Retain the next line for continuation without exposing it outside this window's coverage.
                SourceRecord::Leaf(_) => None,
            }))
    }

    pub fn ordinary_leaves_for<'a>(
        &'a self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<
        impl Iterator<Item = &'a VerifiedMarkdownOrdinaryLeaf> + use<'a>,
        MarkdownTableRequestError,
    > {
        self.check(document, authority)?;
        Ok(self
            .records
            .iter()
            .filter_map(|record| match record.as_ref() {
                SourceRecord::Leaf(leaf)
                    if leaf.physical_utf16.start < self.requested.end
                        && self.requested.start < leaf.physical_utf16.end =>
                {
                    Some(leaf)
                }
                SourceRecord::Leaf(_) => None, // A future original fact waits for an actual successor intersection rather than widening this publication.
                SourceRecord::Line(_) => None,
            }))
    }

    /// The producer consumes closing-fence LF/CRLF outside the Parser's CodeBlock style range.
    /// Lend the same Root leaf's actual separator only for a complete control range, without extending inline coverage or hiding authority.
    pub fn root_fence_separator_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
        range: &Range<usize>,
    ) -> Result<Option<Range<usize>>, MarkdownTableRequestError> {
        self.check(document, authority)?;
        if range.is_empty() || range.start < self.requested.start || range.end > self.requested.end
        {
            return Ok(None); // Even a real line outside the requested envelope cannot obtain control authority from this publication.
        }
        for record in &self.records {
            let SourceRecord::Leaf(leaf) = record.as_ref() else {
                continue;
            };
            if leaf.block.scope_depth() != 0
                || leaf.kind() != LeafBlockKind::FencedCode
                || leaf.close_reason() != LeafBlockCloseReason::ExplicitMarker
            {
                continue; // Container, EOF, and other leaf cursors do not satisfy this Root closing-EOL contract.
            }
            let length = leaf.consumed_end() - leaf.source_range().end;
            if length == 0 {
                continue; // A closer at actual EOF has no separator; never fabricate an empty control.
            }
            // The firstpass closer stops before CR/LF; scan_blank_line consumes only that same LF/CRLF.
            // These ASCII bytes map one-to-one to UTF-16 units, so measured physical endpoints need no text reread or copy.
            let end = leaf.physical_utf16.end;
            let separator = end - length..end;
            if *range == separator {
                return Ok(Some(separator)); // CRLF is one actual control; requesting only half cannot authorize its other half.
            }
        }
        Ok(None)
    }
}

struct InlineJob {
    cursor: Box<BlockOriginCursor>,
    kind: InlineBlockKind,
    lexical: Range<usize>,
    semantic_end: usize,
    // The Setext terminal line is outside content Origin; measure its actual closing tail before releasing the sole origin cursor.
    tail_pending: bool,
    terminal_coordinates: Option<[usize; 4]>,
    pending_line: Option<BlockOriginLine>,
    semantic_block: Option<Rc<OrdinaryBlockRecord>>,
}

enum OrdinaryJob {
    Inline(InlineJob),
    Leaf(VerifiedGlobalLeafBlock),
}

#[derive(Clone, Copy)]
enum SourcePhase {
    RestartLeaf,
    Produce,
    Available,
    Consumed,
}

/// Wrap one original table batch without a second structural Global or a Parser restarted from a gap.
/// The table batch owns classification progress; the sole origin cursor owns origin_floor, and neither substitutes for the other.
pub struct MarkdownViewportSourceRequest {
    batch: MarkdownTableViewportRequest,
    source: MarkdownTableSourceIdentity,
    context_revision: u64,
    selection: MarkdownTextSelection,
    // Complete selection is immutable for this request; retain only two proved byte endpoints rather than text or another directory.
    selection_bytes: Option<Range<usize>>,
    // Row-source recovery locates one active query point with the same cumulative source account, without copying row ranges.
    row_source_position: Option<semantic::PositionLocator>,
    viewport: Range<usize>,
    job: Option<OrdinaryJob>,
    coordinates: Option<Coordinates>,
    coordinate_active: bool,
    origin_floor: usize,
    records: Vec<Rc<SourceRecord>>,
    blocks: Vec<Rc<OrdinaryBlockRecord>>,
    semantic: Option<PlainSemanticJob>,
    // Standalone Plain and joint RootLeaf modes are exclusive; after readiness retain fixed job coordinates only, with styles/publication transferred.
    leaf_mode: bool,
    leaf_semantic: Option<Box<RootLeafSemanticJob>>,
    leaf_offer_index: usize,
    ready: Option<VerifiedMarkdownViewportSources>,
    last_publication: Option<Weak<SourcePublication>>,
    phase: SourcePhase,
    failed: Option<MarkdownTableRequestError>,
    cell_inline: Option<Box<cell_inline::CellInlineJob>>,
    // Leaf/cell publication moves styles out; serially transfer the original HTML frontier within this Source request instead of restarting it.
    source_underline:
        Option<crate::markdown_document_buffer::table_source::MarkdownTableUnderlineRequest>,
}

impl MarkdownDocumentBuffer {
    /// Prepare actual intersections of ordinary blocks, headings, code, and separators within this same source request.
    ///
    /// Ready grants only `covered_ranges_for`; unknown syntax in a mixed window remains a hole.
    /// LeafPending means no intersection was proved, and the requested envelope alone never establishes complete semantics.
    pub fn viewport_leaf_source_request(
        &self,
        viewport: Range<usize>,
        context_revision: u64,
        selection: MarkdownTextSelection,
    ) -> Result<MarkdownViewportSourceRequest, MarkdownTableRequestError> {
        let mut request =
            self.viewport_source_request(viewport.clone(), context_revision, selection)?;
        request.leaf_mode = true;
        request.batch.enable_structural_tables();
        request.install_leaf_semantic(viewport)?;
        Ok(request)
    }

    /// Explicit Plain/Setext entry; unsupported complex syntax or non-Root cases remain explicitly Pending where no verified path applies.
    pub fn viewport_plain_source_request(
        &self,
        viewport: Range<usize>,
        context_revision: u64,
        selection: MarkdownTextSelection,
    ) -> Result<MarkdownViewportSourceRequest, MarkdownTableRequestError> {
        let mut request =
            self.viewport_source_request(viewport.clone(), context_revision, selection)?;
        request.semantic = Some(PlainSemanticJob::new(
            viewport,
            selection,
            request.batch.source_semantic_quota()?,
        ));
        Ok(request)
    }

    /// Capture ordinary sources only by explicit opt-in, with the exact current Core selection rather than its ordered primary range.
    pub fn viewport_source_request(
        &self,
        viewport: Range<usize>,
        context_revision: u64,
        selection: MarkdownTextSelection,
    ) -> Result<MarkdownViewportSourceRequest, MarkdownTableRequestError> {
        if selection != self.selection() {
            return Err(MarkdownTableRequestError::PublicationSuperseded);
        }
        let mut batch = self.table_viewport_request(
            viewport.clone(),
            context_revision,
            ordered_selection(selection),
        )?;
        batch.enable_source_capture();
        // Reserve workspace once; this logical amount does not replace whole-product verification of resident pages or allocator memory.
        batch.admit_source_metadata(
            source_workspace_metadata() + publication_reservation_metadata(),
        )?;
        let source = batch.source_identity();
        Ok(MarkdownViewportSourceRequest {
            batch,
            source,
            context_revision,
            selection,
            selection_bytes: None,
            row_source_position: None,
            viewport,
            job: None,
            coordinates: None,
            coordinate_active: false,
            origin_floor: 0,
            records: Vec::new(),
            blocks: Vec::new(),
            semantic: None,
            leaf_mode: false,
            leaf_semantic: None,
            leaf_offer_index: 0,
            ready: None,
            last_publication: None,
            phase: SourcePhase::Produce,
            failed: None,
            cell_inline: None,
            source_underline: None,
        })
    }
}

impl MarkdownViewportSourceRequest {
    fn check(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<(), MarkdownTableRequestError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        check_authority(
            &self.source,
            self.context_revision,
            self.selection,
            document,
            authority,
        )
    }

    fn fail(&mut self, error: MarkdownTableRequestError) {
        // Retire only this owner's continuation; exported immutable metadata retains its original source/context read checks.
        self.failed = Some(error);
        self.cell_inline = None;
        self.source_underline = None;
        self.row_source_position = None;
        self.job = None;
        self.coordinates = None;
        self.coordinate_active = false;
        self.records = Vec::new();
        self.blocks = Vec::new();
        self.semantic = None;
        if error == MarkdownTableRequestError::Cancelled
            && let Some(job) = self.leaf_semantic.as_mut()
        {
            // Explicit cancellation retires the leaf continuation too; its derived pages are released with the request's active state.
            job.cancel();
        }
        self.leaf_semantic = None;
        self.leaf_offer_index = 0;
        self.ready = None;
        self.last_publication = None;
        self.batch.abort_source_request(error);
    }

    pub fn cancel(&mut self) {
        if self.failed.is_none() {
            // Preserve an existing first failure while cancellation releases new derived capacity; old generations cannot revive this owner.
            self.fail(MarkdownTableRequestError::Cancelled);
        }
    }

    pub fn total_work(&self) -> SemanticWork {
        self.batch.total_work()
    }

    /// Obtain a complete first-scalar query window for row recovery; the window itself proves no table or cell membership.
    /// Locate incrementally on this request's original piece tree, charging even the at-most-four-byte scalar read to its account.
    /// The caller must then continue/reposition from this request's latest publication to acquire an actual row ticket.
    pub fn table_row_source_viewport(
        &mut self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
        position: usize,
        budget: usize,
    ) -> Result<Option<Range<usize>>, MarkdownTableRequestError> {
        let result = (|| {
            document
                .original_backing_health()
                .map_err(MarkdownTableRequestError::Source)?;
            self.check(document, authority)?;
            if !self.leaf_mode
                || !matches!(self.phase, SourcePhase::Consumed)
                || self.cell_inline.is_some()
                || position >= document.utf16_len()
                || !(4096..=65536).contains(&budget)
                || self
                    .row_source_position
                    .as_ref()
                    .is_some_and(|locator| !locator.targets(position))
            {
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            let (bytes, transitions) = self.remaining_source_limits()?;
            let quantum = budget.min(bytes).min(transitions);
            if quantum < 4096 {
                return Err(MarkdownTableRequestError::WorkLimit);
            }
            let locator = self
                .row_source_position
                .get_or_insert_with(|| semantic::PositionLocator::new(position, false));
            let (work, result) = locator.advance_row_source_viewport(document, quantum);
            let charged = self.batch.account_source_work(work);
            // Original source failure takes priority after frame return; charge actual scalar reads even when decoding or return fails.
            document
                .original_backing_health()
                .map_err(MarkdownTableRequestError::Source)?;
            let viewport = result?;
            charged?;
            if viewport.is_some() {
                self.row_source_position = None;
            }
            Ok(viewport)
        })();
        if let Err(error) = result {
            self.fail(error); // Location and ordinary-source failures share this owner; preserve neither half a query nor reissued fuel.
        }
        result
    }

    /// Report only measured ordinary-line/leaf progress, independently of Global's classification frontier.
    pub fn origin_forward_floor(&self) -> usize {
        self.origin_floor
    }

    pub fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
        budget: usize,
    ) -> Result<MarkdownViewportSourceStep, MarkdownTableRequestError> {
        let result = self.check(document, authority).and_then(|()| {
            if !(4096..=65536).contains(&budget) {
                // A public step must cover minimum actual control/coordinate work; an invalid budget grants no output authority.
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            if matches!(self.phase, SourcePhase::Available) {
                // Observe the same completed source publication without reallocating, recounting, or granting ordinary style readiness.
                return Ok(MarkdownViewportSourceStep::SourcesAvailable {
                    work: SemanticWork::default(),
                });
            }
            if matches!(self.phase, SourcePhase::Consumed) {
                // After transfer, resume explicitly with the original publication; ordinary advance cannot bypass continuation checks.
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            let (max_bytes, max_transitions) = self.batch.source_limits()?;
            let total = self.batch.total_work();
            let quantum = budget
                .min(max_bytes.saturating_sub(total.source_bytes))
                .min(max_transitions.saturating_sub(total.structural_transitions));
            if quantum < 4096 {
                // Fail when remaining cumulative fuel cannot cover a legal public step; later rows/batches do not renew the total.
                return Err(MarkdownTableRequestError::WorkLimit);
            }
            self.advance_inner(document, quantum)
        });
        if let Err(error) = result {
            // This mutating boundary fixes the first failure and releases derived state; restoring old flags/revisions cannot revive it.
            self.fail(error);
        }
        result
    }

    fn advance_inner(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<MarkdownViewportSourceStep, MarkdownTableRequestError> {
        if matches!(self.phase, SourcePhase::RestartLeaf) {
            // Charge Global reposition control and new Leaf inventory in separate original 4096-unit steps rather than combining both constructions.
            self.install_leaf_semantic(self.viewport.clone())?;
            self.phase = SourcePhase::Produce;
            return Ok(MarkdownViewportSourceStep::Progress {
                work: RootLeafSemanticJob::construction_work(),
            });
        }
        if self.leaf_mode
            && let Some(index) = self
                .semantic
                .as_ref()
                .filter(|job| !job.done())
                .and_then(PlainSemanticJob::prefix_origin_record_index)
        {
            // Borrow one current source record without copying the Origin directory or creating another Parser; resume next step if unmatched.
            let record = self
                .records
                .get(index)
                .ok_or(MarkdownTableRequestError::InvalidRequest)?;
            let work = self
                .semantic
                .as_mut()
                .unwrap()
                .advance_prefix_origin(record)?;
            self.batch.account_source_work(work)?;
            return Ok(MarkdownViewportSourceStep::Progress { work });
        }
        if self
            .leaf_semantic
            .as_ref()
            .is_some_and(|job| job.preparing())
        {
            // Measure all four endpoints incrementally through the original piece root before deciding which opaque leaf owns coverage.
            let authority = self.current_authority();
            let limits = self.remaining_source_limits()?;
            let work = self.leaf_semantic.as_mut().unwrap().advance(
                document,
                authority,
                budget,
                limits,
                &mut self.semantic,
                Some(self.batch.source_global_mut()?),
            )?;
            self.batch.account_source_work(work)?;
            return Ok(MarkdownViewportSourceStep::Progress { work });
        }
        if let Some(job) = self.leaf_semantic.as_mut()
            && self.leaf_offer_index < self.records.len()
        {
            // Offer retained and newly captured records one per step instead of traversing the whole 256-record page in one call.
            let work = job.offer_record(&self.records[self.leaf_offer_index])?;
            self.leaf_offer_index += 1;
            self.batch.account_source_work(work)?;
            return Ok(MarkdownViewportSourceStep::Progress { work });
        }
        if !self.leaf_mode
            && self
                .semantic
                .as_ref()
                .is_some_and(PlainSemanticJob::preparing)
        {
            // Locate actual viewport/selection endpoints first, then retain original Fact seeds and global underline context for finite byte coverage.
            let mut work = self
                .semantic
                .as_mut()
                .unwrap()
                .advance_bounds(document, budget - CONTROL_COST - 2 * MAX_SOURCE_RECORDS)?;
            if !self.semantic.as_ref().unwrap().preparing() {
                let semantic = self.semantic.as_mut().unwrap();
                work.structural_transitions += 2 * self.blocks.len();
                self.blocks
                    .retain(|block| semantic.considers(&block.seed.physical_range()));
                for block in &self.blocks {
                    semantic.consider(block);
                }
            }
            work.structural_transitions += CONTROL_COST;
            self.batch.account_source_work(work)?;
            return Ok(MarkdownViewportSourceStep::Progress { work });
        }
        if self.batch.ready_for(document)?.is_some() {
            if self.leaf_semantic.as_ref().is_some_and(|job| !job.done()) {
                // Prepare leaf semantics only after table/source closure, advancing one bounded stage and charging the original account.
                let authority = self.current_authority();
                let limits = self.remaining_source_limits()?;
                let work = self.leaf_semantic.as_mut().unwrap().advance(
                    document,
                    authority,
                    budget,
                    limits,
                    &mut self.semantic,
                    Some(self.batch.source_global_mut()?),
                )?;
                self.batch.account_source_work(work)?;
                return Ok(MarkdownViewportSourceStep::Progress { work });
            }
            if !self.leaf_mode
                && self
                    .semantic
                    .as_ref()
                    .is_some_and(|semantic| !semantic.done())
            {
                // Pay semantic steps after table/source completion; source progress or empty styles cannot substitute for PlainReady.
                if self.semantic.as_ref().unwrap().needs_underline_workspace() {
                    self.batch.reserve_source_underline_workspace()?;
                }
                let limits = self.batch.source_limits()?;
                let total = self.batch.total_work();
                let (work, runs) = self.semantic.as_mut().unwrap().advance_after_sources(
                    document,
                    budget,
                    (
                        limits.0.saturating_sub(total.source_bytes),
                        limits.1.saturating_sub(total.structural_transitions),
                    ),
                    Some(self.batch.source_global_mut()?),
                )?;
                // Newly built runs remain private until the same quota admits them; no consumer receives an unadmitted result.
                self.batch.admit_source_runs(runs)?;
                self.batch.account_source_work(work)?;
                return Ok(MarkdownViewportSourceStep::Progress { work });
            }
            // A table stage may consume the preceding step entirely; source publication pays its own control and metadata work.
            return self.publish_sources(document, budget);
        }
        if self.job.is_some() && self.origin_floor < self.viewport.end {
            // Advance the sole original origin cursor first; even Global's no-table classification reaching EOF cannot skip its physical lines.
            let work = self.advance_ordinary(document, budget)?;
            self.batch.account_source_work(work)?;
            return Ok(MarkdownViewportSourceStep::Progress { work });
        }
        if let Some(captured) = self.batch.take_ordinary_source() {
            // Accept only opaque facts from this actual Global, never paragraph/leaf seeds manufactured from gap coordinates.
            if self.job.is_some() {
                // Classification should seal the batch when unfinished origin covers the viewport; another Fact cannot create a second active cursor.
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            let work = self.install_job(captured)?;
            self.batch.account_source_work(work)?;
            return Ok(MarkdownViewportSourceStep::Progress { work });
        }
        // Advance the original table batch only when no captured source waits; it accounts for its own Global/reference work.
        let step = self.batch.advance(document, false, budget)?;
        let work = match step {
            MarkdownTableViewportStep::Progress { work }
            | MarkdownTableViewportStep::Ready { work } => work,
        };
        Ok(MarkdownViewportSourceStep::Progress { work })
    }

    fn publish_sources(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<MarkdownViewportSourceStep, MarkdownTableRequestError> {
        let leaf_work = self
            .leaf_semantic
            .as_ref()
            .map_or(Ok(SemanticWork::default()), |job| {
                job.publication_work(document)
            })?;
        // A leaf's semantic slot retains an already transferred Plain prefix; do not publish another Plain Box or clean pairs twice.
        let plain = (!self.leaf_mode)
            .then_some(self.semantic.as_ref())
            .flatten();
        let work = SemanticWork {
            // Charge fixed Plain Box control only for actual creation; original publication reservation already covers its payload and handle.
            structural_transitions: 2 * CONTROL_COST
                + plain.map_or(Ok(0), |job| job.publication_control(document))?
                + leaf_work.structural_transitions,
            metadata_bytes: publication_metadata()
                + plain.map_or(0, PlainSemanticJob::publication_payload_metadata)
                + leaf_work.metadata_bytes,
            ..SemanticWork::default()
        };
        if work.structural_transitions > budget || work.metadata_bytes > budget {
            // Admit actual publication transfer and metadata first; insufficient work budget must not create Rc or move table results.
            return Err(MarkdownTableRequestError::WorkLimit);
        }
        self.batch.account_source_work(work)?;
        // Transfer table results and the source directory after the same batch succeeds; Sources still grants no implicit ordinary readiness.
        let table = self
            .batch
            .take_ready_for(document)?
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        let publication = Rc::new(SourcePublication {
            source: self.source.clone(),
            context_revision: self.context_revision,
            selection: self.selection,
            retired: Cell::new(false),
        });
        // Recover this source/complete selection's actual endpoints before transferring semantics. Later cells still check publication,
        // but need not repeatedly decode prefixes to a distant Find selection; these scalars grant no extra semantic coverage.
        if self.selection_bytes.is_none() {
            self.selection_bytes = self
                .leaf_semantic
                .as_ref()
                .and_then(|job| job.verified_selection_bytes())
                .or_else(|| {
                    self.semantic
                        .as_ref()
                        .and_then(PlainSemanticJob::verified_selection_bytes)
                });
        }
        let authority = self.current_authority();
        let semantic = if let Some(job) = self.leaf_semantic.as_mut() {
            // Bind readiness to this SourcePublication; after output transfer retain fixed coordinates without an old publication chain.
            let (published, measured) =
                job.publish(document, authority, Rc::downgrade(&publication), budget)?;
            if measured != leaf_work {
                // No callback occurs between these Core publication steps; inconsistent work means an internal stage mismatch, not partial success.
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            self.source_underline = job.return_underline();
            match published {
                RootLeafSemanticPublication::Pending(reason) => {
                    // Pending means this window has no publishable semantics, without revoking coordinates for the same source/selection.
                    // The sole job has transferred output and reached Published; successors still prove their own ranges/source.
                    // Retain its subpiece frontier rather than restarting UTF-8 prefix decoding after every empty LF.
                    MarkdownOrdinarySemanticState::LeafPending(reason)
                }
                RootLeafSemanticPublication::Ready(ready) => {
                    MarkdownOrdinarySemanticState::RootLeafReady(ready)
                }
            }
        } else {
            match self.semantic.as_mut() {
                Some(job) => job.finish(
                    self.source.clone(),
                    self.context_revision,
                    Rc::downgrade(&publication),
                )?,
                None => MarkdownOrdinarySemanticState::SourcesOnly,
            }
        };
        let retained_metadata = self
            .records
            .len()
            .checked_mul(record_metadata())
            .and_then(|bytes| bytes.checked_add(publication_metadata()))
            .and_then(|bytes| bytes.checked_add(semantic.retained_metadata()))
            .and_then(|bytes| {
                bytes.checked_add(
                    self.blocks.len() * block_metadata()
                        + self.blocks.capacity() * std::mem::size_of::<Rc<OrdinaryBlockRecord>>(),
                )
            })
            .ok_or(MarkdownTableRequestError::WorkLimit)?;
        self.last_publication = Some(Rc::downgrade(&publication));
        self.ready = Some(VerifiedMarkdownViewportSources {
            table,
            records: std::mem::take(&mut self.records),
            requested: self.viewport.clone(),
            publication,
            retained_metadata,
            blocks: std::mem::take(&mut self.blocks),
            semantic,
        });
        self.phase = SourcePhase::Available;
        Ok(MarkdownViewportSourceStep::SourcesAvailable { work })
    }

    fn install_job(
        &mut self,
        captured: CapturedOrdinary,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let mut work = SemanticWork {
            structural_transitions: CONTROL_COST,
            ..SemanticWork::default()
        };
        self.job = Some(match captured {
            CapturedOrdinary::Inline(block) => {
                // Only a real closed ticket creates origin; the child uses this request's remaining limits and returns work to its account.
                let kind = block.kind();
                let lexical = block.lexical_range();
                let semantic_end = block.source_range().end;
                let (max_bytes, max_transitions) = self.batch.source_limits()?;
                let total = self.batch.total_work();
                let limits = BlockOriginLimits {
                    max_byte_work: max_bytes.saturating_sub(total.source_bytes),
                    max_transitions: max_transitions.saturating_sub(total.structural_transitions),
                };
                let keep = (!self.leaf_mode
                    && self
                        .semantic
                        .as_ref()
                        .is_some_and(|job| job.considers(&block.physical_range())))
                    || self
                        .leaf_semantic
                        .as_ref()
                        .is_some_and(|job| job.retains_inline_seed(&block.physical_range()));
                let (cursor, semantic_block) = if keep {
                    if self.blocks.len() == MAX_SOURCE_RECORDS {
                        return Err(MarkdownTableRequestError::Window(
                            MarkdownTableWindowError::SemanticLimit,
                        ));
                    }
                    // Admit joint inventory before allocating Rc/directory capacity; seeds retain neither full text, role sets, nor strong Source owners.
                    self.batch.admit_source_metadata(
                        block_metadata() + std::mem::size_of::<Rc<OrdinaryBlockRecord>>(),
                    )?;
                    work.metadata_bytes = block_metadata()
                        + self.blocks.len() * std::mem::size_of::<Rc<OrdinaryBlockRecord>>();
                    work.structural_transitions += self.blocks.len() + CONTROL_COST;
                    let (cursor, seed) = block
                        .into_origins_and_semantic_seed(limits)
                        .map_err(MarkdownTableRequestError::Global)?;
                    let record = Rc::new(OrdinaryBlockRecord { seed });
                    if !self.leaf_mode
                        && let Some(semantic) = self.semantic.as_mut()
                    {
                        semantic.consider(&record);
                    } // Joint mode receives the same seed from its original physical line instead of creating another Plain request.

                    self.blocks.reserve_exact(1);
                    self.blocks.push(Rc::clone(&record));
                    (cursor, Some(record))
                } else {
                    // Preserve the original factory for SourcesOnly/out-of-window facts without allocating semantics for unrelated blocks.
                    (
                        block
                            .into_line_origins(limits)
                            .map_err(MarkdownTableRequestError::Global)?,
                        None,
                    )
                };
                OrdinaryJob::Inline(InlineJob {
                    cursor: Box::new(cursor),
                    kind,
                    lexical,
                    semantic_end,
                    tail_pending: false,
                    terminal_coordinates: None,
                    pending_line: None,
                    semantic_block,
                })
            }
            CapturedOrdinary::Leaf(block) => {
                // Retain the opaque leaf until actual source coordinates finish, without reclassification or a body rescan.
                OrdinaryJob::Leaf(block)
            }
        });
        Ok(work)
    }

    fn advance_ordinary(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let needs_coordinates = match self.job.as_ref().unwrap() {
            OrdinaryJob::Inline(job) => {
                // Finish coordinates for the pending physical line before another line could replace that sole transfer slot.
                job.pending_line.is_some() || job.tail_pending
            }
            OrdinaryJob::Leaf(_) => true, // The actual leaf ticket is already closed and only needs this source's UTF-16 coordinates.
        };
        if needs_coordinates {
            // Prefer the continuous coordinate cursor's existing boundaries instead of rebuilding and rescanning the same leaf prefix per line.
            return self.advance_coordinates(document, budget);
        }
        let tree = tree_height(document);
        let child = child_budget(budget - CONTROL_COST, tree)?;
        let OrdinaryJob::Inline(job) = self.job.as_mut().unwrap() else {
            unreachable!()
        };
        let frame = document
            .table_source_frame(job.cursor.next_byte(), child)
            .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
        let step = job
            .cursor
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
        // Consume parser progress only after successful frame return, so early parser completion cannot mask a mapping failure.
        frame
            .finish()
            .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
        let step = step?;
        let (measured, complete) = match step {
            BlockOriginStep::Progress { work, .. } => {
                // The sole cursor is still matching container/content; charge actual work without publishing an incomplete physical line.
                (work, false)
            }
            BlockOriginStep::NeedInput { work, .. } => {
                // A frame end requests another borrow at the original position; it is neither source EOF nor completed coverage.
                (work, false)
            }
            BlockOriginStep::Line { line, work } => {
                // Retain this one finite origin record, convert actual absolute boundaries, then decide whether it intersects the viewport.
                job.pending_line = Some(line);
                (work, false)
            }
            BlockOriginStep::Complete { work } => {
                // Exhausting a real closed block's origin grants neither ordinary inline styles nor edit readiness.
                (work, true)
            }
        };
        if complete {
            // Release the capture only after actual consumption; subsequent blocks continue from the same Global frontier.
            self.job = None;
        }
        Ok(SemanticWork {
            source_bytes: measured.byte_reads,
            source_tree_steps: tree,
            structural_transitions: measured.transitions + tree + 2 * CONTROL_COST,
            ..SemanticWork::default()
        })
    }

    fn advance_coordinates(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        if !self.coordinate_active {
            let boundaries = match self.job.as_ref().unwrap() {
                OrdinaryJob::Inline(job) => {
                    // Convert the four original prefix/content/EOL byte boundaries monotonically, without normalizing CRLF or Unicode.
                    if job.tail_pending {
                        // The Setext fact already proves its tail range; measure only that closing end without inventing a content line.
                        [job.semantic_end; 4]
                    } else {
                        let line = job.pending_line.as_ref().unwrap();
                        [
                            line.physical_range().start,
                            line.content_range().start,
                            line.content_range().end,
                            line.eol_range().end,
                        ]
                    }
                }
                OrdinaryJob::Leaf(block) => {
                    // source_range.end may include the next line's prefix, so advance shared coordinates only to the actual consumed cursor.
                    // Root Code measures its body with the middle two original endpoints, rather than inferring full-leaf boundaries
                    // from clipped markers or creating another locator/text read for selection invariance.
                    let body = root_code_body(block);
                    [
                        block.physical_start(),
                        body.map_or_else(|| block.source_range().start, |body| body.start),
                        body.map_or_else(|| block.consumed_end(), |body| body.end),
                        block.consumed_end(),
                    ]
                }
            };
            let mut work = if let Some(coordinates) = self.coordinates.as_mut() {
                // Continue following lines/blocks from the same byte-to-UTF-16 frontier, replacing four boundaries without another AVL lookup.
                coordinates.continue_at(boundaries)?
            } else {
                // Initial AVL location has happened synchronously; return its cost now even if cancellation or Drop follows.
                let (coordinates, work) =
                    Coordinates::new(document, boundaries, budget - CONTROL_COST)?;
                self.coordinates = Some(coordinates);
                work
            };
            self.coordinate_active = true;
            work.structural_transitions = work
                .structural_transitions
                .checked_add(CONTROL_COST)
                .ok_or(MarkdownTableRequestError::WorkLimit)?;
            return Ok(work); // Account for synchronous location in this call rather than deferring its report to another advance.
        }
        let (mut work, values) = self
            .coordinates
            .as_mut()
            .unwrap()
            .advance(document, budget - CONTROL_COST)?;
        let Some(values) = values else {
            // An incomplete four-boundary conversion returns actual read work without advancing origin coverage.
            return Ok(work);
        };
        // Row/Rc publication remains in the parent's step; the child reserved this actual transfer control work.
        work.structural_transitions = work
            .structural_transitions
            .checked_add(CONTROL_COST)
            .ok_or(MarkdownTableRequestError::WorkLimit)?;
        self.coordinate_active = false;
        if let Some(OrdinaryJob::Inline(job)) = self.job.as_mut()
            && !job.tail_pending
            && job.pending_line.as_ref().unwrap().is_terminal()
            && job.pending_line.as_ref().unwrap().eol_range().end < job.semantic_end
        {
            // The terminal content line keeps its own four endpoints; next step measures the proved Setext tail through the same cursor.
            // Do not close origin_floor before transferring the complete owner, or the next batch would lose the sole underline seed.
            job.terminal_coordinates = Some(values);
            job.tail_pending = true;
            return Ok(work);
        }
        self.origin_floor = values[3];
        self.batch.set_source_origin_floor(self.origin_floor);
        let (record, complete) = match self.job.as_mut().unwrap() {
            OrdinaryJob::Inline(job) => {
                // Transfer the original line with its four coordinates; release unfinished origin only after converting the terminal line.
                let origin = job.pending_line.take().unwrap();
                let complete = origin.is_terminal();
                let physical = if job.tail_pending {
                    job.terminal_coordinates
                        .take()
                        .expect("原 Setext 尾部与自己的 terminal 行四端点一起移交")
                } else {
                    values // An ordinary line's physical and measured endpoints coincide; it has no additional syntax tail.
                };
                (
                    SourceRecord::Line(VerifiedMarkdownOrdinaryLine {
                        block_kind: job.kind,
                        block_lexical: job.lexical.clone(),
                        origin,
                        utf16: physical,
                        semantic_utf16_end: values[3],
                        semantic_block: job.semantic_block.as_ref().map(Rc::clone),
                    }),
                    complete,
                )
            }
            OrdinaryJob::Leaf(_) => {
                // Move the original opaque leaf instead of rebuilding a ticket, preserving its distinct source/consumed ranges.
                let Some(OrdinaryJob::Leaf(block)) = self.job.take() else {
                    unreachable!()
                };
                #[cfg(feature = "paged-line-compose")]
                let root_code_body = root_code_body(&block).map(|_| RootCodeBody {
                    utf16: values[1]..values[2],
                });
                (
                    SourceRecord::Leaf(VerifiedMarkdownOrdinaryLeaf {
                        block,
                        physical_utf16: values[0]..values[3],
                        #[cfg(feature = "paged-line-compose")]
                        root_code_body,
                    }),
                    true,
                )
            }
        };
        let range = record.utf16_range();
        // Retire earlier lines as scanning advances, but keep the first already consumed fact beyond a whitespace window boundary.
        // origin_floor already passed that fact, so dropping it would prevent the same Global from delivering its source again.
        // Retain only this first future line/leaf within the original 256-record quota; getters and semantic coverage still filter actual intersections.
        if self.viewport.start < range.end {
            work.metadata_bytes = work
                .metadata_bytes
                .checked_add(record_metadata())
                .ok_or(MarkdownTableRequestError::WorkLimit)?;
            self.admit_record(record)?;
        }
        if complete {
            // Release the cursor at the actual terminal line/leaf before allowing the same Global to deliver another ordinary fact.
            self.job = None;
        }
        Ok(work)
    }

    fn admit_record(&mut self, record: SourceRecord) -> Result<(), MarkdownTableRequestError> {
        if self.records.len() == MAX_SOURCE_RECORDS {
            // Reject a source page beyond 256 records rather than accumulating every physical line of an entire paragraph.
            return Err(MarkdownTableRequestError::Window(
                MarkdownTableWindowError::SemanticLimit,
            ));
        }
        // Continuation already charged previous payload; jointly admit each new record before Rc allocation without retaining a giant line collection.
        self.batch.admit_source_metadata(record_metadata())?;
        self.records.push(Rc::new(record));
        Ok(())
    }

    pub fn sources_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<Option<&VerifiedMarkdownViewportSources>, MarkdownTableRequestError> {
        self.check(document, authority)?;
        Ok(self.ready.as_ref())
    }

    pub fn take_sources_for(
        &mut self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<Option<VerifiedMarkdownViewportSources>, MarkdownTableRequestError> {
        if let Err(error) = self.check(document, authority) {
            self.fail(error);
            return Err(error);
        }
        let sources = self.ready.take();
        if sources.is_some() {
            self.phase = SourcePhase::Consumed;
        }
        Ok(sources)
    }

    /// Reproject retained closed ordinary lines or a real Root Code leaf when only this document's selection changes.
    /// Ok(false) means retained inventory cannot cover the target and changes no owner, publication, or proof.
    /// Success still requires incremental advance and a new publication for the new selection; old Ready/Reader handles cannot be relabeled.
    ///
    /// # Errors
    /// Check source health, cancellation, complete current selection, and latest publication separately; post-admission failure fixes the first cause and ends the request.
    pub fn reproject_selection(
        &mut self,
        document: &MarkdownDocumentBuffer,
        previous: &VerifiedMarkdownViewportSources,
        viewport: Range<usize>,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<bool, MarkdownTableRequestError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        // The root endpoint invokes original health only. The old selection is no longer current, so an old-selection check
        // must not turn PublicationSuperseded into absence of capability or bypass the actual mapping failure/cancellation.
        let current = document
            .byte_offset_for_utf16(0)
            .map_err(MarkdownTableRequestError::Source)
            .and_then(|_| {
                check_authority(
                    &self.source,
                    self.context_revision,
                    authority.selection,
                    document,
                    authority,
                )
            });
        if let Err(error) = current {
            self.fail(error);
            return Err(error);
        }
        if !matches!(self.phase, SourcePhase::Consumed)
            || previous.publication.retired.get()
            || self.context_revision != previous.publication.context_revision
            || self.selection != previous.publication.selection
            || !self
                .last_publication
                .as_ref()
                .is_some_and(|last| last.ptr_eq(&Rc::downgrade(&previous.publication)))
        {
            return Err(MarkdownTableRequestError::PublicationSuperseded);
        }
        if viewport.is_empty() || viewport.end > document.utf16_len() || viewport.len() > 64 * 1024
        {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        if !self.leaf_mode
            || self.job.is_some()
            || self.coordinate_active
            || self.cell_inline.is_some()
            || self.row_source_position.is_some()
            || !matches!(
                &previous.semantic,
                MarkdownOrdinarySemanticState::RootLeafReady(_)
            )
        {
            return Ok(false); // Active source/cell jobs own their cleanup obligations; selection changes cannot truncate them.
        }
        let Some((index, record)) = previous.records.iter().enumerate().find(|(_, record)| {
            let range = record.utf16_range();
            range.start <= viewport.start
                && viewport.end <= range.end
                && match record.as_ref() {
                    SourceRecord::Line(line) => line.semantic_block.is_some(),
                    SourceRecord::Leaf(leaf) => root_code_body(&leaf.block).is_some(),
                }
        }) else {
            return Ok(false); // A released prefix or unknown intervening block needs the original cold path, not an invented historical line directory.
        };
        let block = match record.as_ref() {
            SourceRecord::Line(line) => line.semantic_block.as_ref(),
            // Code is not an OrdinaryBlock seed. Retain the same opaque leaf Rc and use RestartLeaf's original
            // coordinate/semantic flow to publish the new selection, including renewed marker visibility.
            SourceRecord::Leaf(_) => None,
        };
        let result = (|| {
            if !self.batch.reproject_source_selection(
                document,
                &previous.table,
                viewport.clone(),
                authority.context_revision,
                ordered_selection(authority.selection),
            )? {
                return Ok(false);
            }
            // previous remains live until return. Shared rows/seeds add only a finite Rc directory; jointly admit the new
            // Leaf slot/publication through original workspace reservations without copying text or renewing cumulative fuel.
            self.batch.admit_source_metadata(
                previous.retained_metadata
                    + std::mem::size_of::<Rc<SourceRecord>>()
                    + usize::from(block.is_some()) * std::mem::size_of::<Rc<OrdinaryBlockRecord>>()
                    + source_workspace_metadata()
                    + publication_reservation_metadata(),
            )?;
            self.batch
                .admit_source_runs(previous.semantic.run_count())?;
            let keep_plain = block.is_some_and(|block| {
                self.semantic
                    .as_ref()
                    .is_some_and(|job| job.retains_plain_block(block))
            });
            self.batch.account_source_work(SemanticWork {
                structural_transitions: CONTROL_COST
                    + index
                    + 1
                    + usize::from(self.leaf_semantic.is_some())
                        * RootLeafSemanticJob::completed_owner_drop_control()
                    + usize::from(self.semantic.is_some() && !keep_plain)
                        * PlainSemanticJob::completed_drop_control(),
                ..Default::default()
            })?;
            // After all fallible admissions succeed, retire the original publication. Old Rc/weak semantic tickets cannot
            // revive when selection values recur; only normal publication creates the new SourcePublication.
            previous.publication.retired.set(true);
            if !keep_plain {
                self.semantic = None;
            }
            self.leaf_semantic = None;
            self.records = vec![Rc::clone(record)];
            self.blocks = block.map_or_else(Vec::new, |block| vec![Rc::clone(block)]);
            self.selection = authority.selection;
            self.context_revision = authority.context_revision;
            self.selection_bytes = None; // Locate the new selection's four endpoints; an old byte pair grants no successor authority.
            self.viewport = viewport;
            self.ready = None;
            self.last_publication = None;
            self.leaf_offer_index = 0;
            self.phase = SourcePhase::RestartLeaf;
            // Preserve the sole underline owner's ResumeUnderline protocol: backward coverage rejects continuation and
            // returns the old scanner in its own step before cold preparation; empty pairs never prove document-wide absence.
            Ok(true)
        })();
        if let Err(error) = result {
            self.fail(error);
        }
        result
    }

    /// Reuse only the latest original publication and sole unfinished origin cursor; backward coverage without a checkpoint is rejected.
    pub fn continue_forward(
        &mut self,
        document: &MarkdownDocumentBuffer,
        previous: &VerifiedMarkdownViewportSources,
        viewport: Range<usize>,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<(), MarkdownTableRequestError> {
        self.check_forward_sources(document, previous, &viewport, authority)?;
        let result = self.begin_forward(document, previous, viewport);
        if let Err(error) = result {
            self.fail(error); // Failure after joint admission/state transition cannot leave a half-accepted continuation.
        }
        result
    }

    /// Restore real RootLeaf semantics for an older viewport by replaying the same Global from root through normal incremental steps.
    ///
    /// # Errors
    /// Require this request's latest transferred source, same Source, and complete selection; reject foreign tickets without mutation, while actual revocation/replay failure remains sticky.
    pub fn reposition(
        &mut self,
        document: &MarkdownDocumentBuffer,
        previous: &VerifiedMarkdownViewportSources,
        viewport: Range<usize>,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<(), MarkdownTableRequestError> {
        if let Some(error) = self.failed {
            return Err(error); // A finished request preserves its first cause rather than replacing it with later source-health changes.
        }
        // Original source I/O failure precedes stale numeric authority; the root endpoint reads no text and must not wash EIO into SourceSuperseded.
        if let Err(cause) = document.byte_offset_for_utf16(0) {
            let error = MarkdownTableRequestError::Source(cause);
            self.fail(error);
            return Err(error);
        }
        if let Err(error) = self.check(document, authority) {
            self.fail(error);
            return Err(error);
        }
        previous.check(document, authority)?;
        if !self.leaf_mode
            || viewport.is_empty()
            || viewport.end > document.utf16_len()
            || viewport.len() > 64 * 1024
        {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        if !matches!(self.phase, SourcePhase::Consumed)
            || self.cell_inline.is_some()
            || self.row_source_position.is_some()
            || !self
                .last_publication
                .as_ref()
                .is_some_and(|last| last.ptr_eq(&Rc::downgrade(&previous.publication)))
        {
            return Err(MarkdownTableRequestError::PublicationSuperseded);
        }
        let result = (|| {
            self.batch
                .reposition_source(document, &previous.table, viewport.clone())?;
            self.batch.admit_source_metadata(
                previous.retained_metadata
                    + source_workspace_metadata()
                    + publication_reservation_metadata(),
            )?;
            self.batch
                .admit_source_runs(previous.semantic.run_count())?;
            if self.semantic.is_some() {
                // Backward reposition releases the parent's completed prefix; structural replay work does not pay for that resource teardown.
                self.batch.account_source_work(SemanticWork {
                    structural_transitions: PlainSemanticJob::completed_drop_control(),
                    ..Default::default()
                })?;
            }
            if self.leaf_semantic.is_some() {
                // Backward movement cannot reuse the subpiece frontier; releasing the sole transferred-output job is actual control work too.
                self.batch.account_source_work(SemanticWork {
                    structural_transitions: RootLeafSemanticJob::completed_owner_drop_control(),
                    ..Default::default()
                })?;
            }
            // Exported old Ready remains readable under its original Source; this request retains no old text origin, semantic scratch, or second frontier.
            self.job = None;
            self.coordinates = None;
            self.coordinate_active = false;
            self.origin_floor = 0;
            self.records = Vec::new();
            self.blocks = Vec::new();
            self.semantic = None;
            self.leaf_semantic = None;
            self.leaf_offer_index = 0;
            self.ready = None;
            self.last_publication = None;
            self.viewport = viewport;
            self.phase = SourcePhase::RestartLeaf;
            Ok(())
        })();
        if let Err(error) = result {
            self.fail(error);
        }
        result
    }

    /// Source and required-row continuation share preflight: actual owner revocation is sticky, while foreign previous/coverage arguments leave state unchanged.
    fn check_forward_sources(
        &mut self,
        document: &MarkdownDocumentBuffer,
        previous: &VerifiedMarkdownViewportSources,
        viewport: &Range<usize>,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<(), MarkdownTableRequestError> {
        // Complete argument/identity preflight before mutation; an incorrect publication cannot damage an otherwise usable original owner.
        if let Err(error) = self.check(document, authority) {
            // This owner's actual source/context is invalid; restoring old numeric values cannot renew its continuation.
            self.fail(error);
            return Err(error);
        }
        previous.check(document, authority)?;
        if !matches!(self.phase, SourcePhase::Consumed)
            || self.cell_inline.is_some()
            || self.row_source_position.is_some()
            || !self
                .last_publication
                .as_ref()
                .is_some_and(|last| last.ptr_eq(&Rc::downgrade(&previous.publication)))
        {
            return Err(MarkdownTableRequestError::PublicationSuperseded);
        }
        if viewport.start < self.viewport.start || viewport.end < self.viewport.end {
            return Err(MarkdownTableRequestError::CoverageRequiresCheckpoint);
        }
        if viewport.start >= viewport.end
            || viewport.end > document.utf16_len()
            || viewport.len() > 64 * 1024
        {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        Ok(())
    }

    fn begin_forward(
        &mut self,
        document: &MarkdownDocumentBuffer,
        previous: &VerifiedMarkdownViewportSources,
        viewport: Range<usize>,
    ) -> Result<(), MarkdownTableRequestError> {
        if viewport.start > self.batch.forward_resume_floor() {
            // An explicit forward jump keeps the original Global, which actually consumes intermediate blocks instead of rebuilding Global(0).
            self.batch
                .resume_source_from_ready(document, &previous.table, viewport.clone())?;
        } else {
            self.batch
                .extend_from_ready(document, &previous.table, viewport.clone())?;
        }
        self.retain_forward_sources(document, previous, viewport)
    }

    /// After the batch accepts its sole successor, jointly admit coexisting ordinary facts and fixed workspace without rebuilding unfinished origin.
    fn retain_forward_sources(
        &mut self,
        document: &MarkdownDocumentBuffer,
        previous: &VerifiedMarkdownViewportSources,
        viewport: Range<usize>,
    ) -> Result<(), MarkdownTableRequestError> {
        let table_handles = previous
            .table
            .windows_for(document)?
            .len()
            .checked_add(previous.table.gaps_for(document)?.len())
            .ok_or(MarkdownTableRequestError::WorkLimit)?;
        let handles = previous
            .records
            .len()
            .checked_mul(std::mem::size_of::<Rc<SourceRecord>>())
            .ok_or(MarkdownTableRequestError::WorkLimit)?;
        self.batch.admit_source_metadata(
            previous
                .retained_metadata
                .checked_add(handles)
                .and_then(|bytes| {
                    bytes.checked_add(
                        previous.blocks.len() * std::mem::size_of::<Rc<OrdinaryBlockRecord>>(),
                    )
                })
                .and_then(|bytes| {
                    bytes.checked_add(
                        source_workspace_metadata() + publication_reservation_metadata(),
                    )
                })
                .ok_or(MarkdownTableRequestError::WorkLimit)?,
        )?;
        self.batch
            .admit_source_runs(previous.semantic.run_count())?;
        // previous still occupies joint quota; clone only a bounded Rc directory, without copying records or retaining a parent-publication chain.
        self.records = previous
            .records
            .iter()
            .filter(|record| {
                let range = record.utf16_range();
                // A leaf may have consumed the sole next line before sealing a whitespace window. Original origin already passed it,
                // so retire only earlier records; future ones await an actual intersection and public getters still clip to the request.
                viewport.start < range.end && (self.leaf_mode || range.start < viewport.end)
            })
            .cloned()
            .collect();
        let block_control = if self.leaf_mode {
            let (blocks, control) = self.retained_leaf_blocks(previous);
            self.blocks = blocks;
            control
        } else {
            self.blocks = previous.blocks.clone();
            2 * previous.blocks.len() // Plain's existing Bounds stage separately prunes blocks against actual byte coverage.
        };
        // Charge old table inventory folding/handle copies too; charging ordinary records alone would omit source-aware resume work.
        let control = previous
            .records
            .len()
            .checked_mul(2)
            .and_then(|count| {
                table_handles
                    .checked_mul(3)
                    .and_then(|tables| count.checked_add(tables))
            })
            .and_then(|count| count.checked_add(CONTROL_COST + block_control))
            .ok_or(MarkdownTableRequestError::WorkLimit)?;
        self.batch.account_source_work(SemanticWork {
            structural_transitions: control,
            ..SemanticWork::default()
        })?;
        if !self.leaf_mode
            && let Some(semantic) = self.semantic.as_mut()
        {
            let work = semantic.retarget(viewport.clone());
            self.batch.account_source_work(work)?;
        }
        if self.leaf_mode {
            // The batch already charged old Ready jointly; the new job borrows the current quota rather than the previous batch's obsolete handle.
            self.install_leaf_semantic(viewport.clone())?;
        }
        self.viewport = viewport;
        self.phase = SourcePhase::Produce;
        self.last_publication = None;
        Ok(())
    }

    /// Retain only opaque seeds still owned by real records, in-flight origin, or the original Plain prefix during leaf continuation.
    /// previous stays immutable and old handles retain their original rights; clone only Rc handles the successor actually needs.
    fn retained_leaf_blocks(
        &self,
        previous: &VerifiedMarkdownViewportSources,
    ) -> (Vec<Rc<OrdinaryBlockRecord>>, usize) {
        let mut blocks = Vec::with_capacity(previous.blocks.len());
        let mut records = self
            .records
            .iter()
            .filter_map(|record| match record.as_ref() {
                SourceRecord::Line(line) => line.semantic_block.as_ref(),
                SourceRecord::Leaf(_) => None, // Actual leaves own no inline seed; adjacent coordinates cannot manufacture one.
            });
        let mut next_record = records.next();
        let mut control = 0;
        // Both directories follow the same Global's real source order. The two cursors skip earlier records and use coordinates
        // only for merging, never identity; retention requires the original Rc or an actual in-flight owner.
        for block in &previous.blocks {
            let start = block.seed.physical_range().start;
            while let Some(record) = next_record {
                control += 1;
                if record.seed.physical_range().start >= start {
                    break;
                }
                next_record = records.next();
            }
            let retained_record = next_record.is_some_and(|record| Rc::ptr_eq(record, block));
            let pending_origin = matches!(
                &self.job,
                Some(OrdinaryJob::Inline(job))
                    if job.semantic_block.as_ref().is_some_and(|current| Rc::ptr_eq(current, block))
            );
            let plain_prefix = self
                .semantic
                .as_ref()
                .is_some_and(|job| job.retains_plain_block(block));
            control += 3; // Check three actual ownership relationships once each, without nested scans of historical directories.
            if retained_record || pending_origin || plain_prefix {
                blocks.push(Rc::clone(block));
                control += 1;
            } // An earlier seed without a successor owner leaves with old previous instead of occupying the next 256-record page.
        }
        (blocks, control)
    }

    /// Called only by the explicit Leaf factory and accepted original forward path, without creating an additional source request.
    fn install_leaf_semantic(
        &mut self,
        viewport: Range<usize>,
    ) -> Result<(), MarkdownTableRequestError> {
        if self
            .semantic
            .as_ref()
            .is_some_and(|job| !job.retains_plain_prefix())
        {
            // Only a Plain prefix whose output and underline were transferred may remain in the parent slot; active semantics cannot run in parallel.
            // The new Leaf first checks Source and Bounds, then reuses this sole slot only after matching the same seed.
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        let quota = self.batch.source_semantic_quota()?;
        let work = if let Some(job) = self.leaf_semantic.as_mut() {
            // Parent forward preflight checked original Source, complete selection, and latest publication; only this published slot may be reused.
            job.continue_forward(viewport, self.source_underline.take(), quota)?
        } else {
            let job = RootLeafSemanticJob::new(
                self.source.clone(),
                self.context_revision,
                self.selection,
                viewport,
                self.selection_bytes.clone(),
                self.source_underline.take(),
                quota,
            )?;
            self.leaf_semantic = Some(Box::new(job));
            RootLeafSemanticJob::construction_work()
        };
        self.batch.account_source_work(work)?;
        self.leaf_offer_index = 0;
        Ok(())
    }

    /// Child semantic continuations use only the original batch's remaining cumulative fuel; new blocks cannot regain its full scan budget.
    fn remaining_source_limits(&self) -> Result<(usize, usize), MarkdownTableRequestError> {
        let limits = self.batch.source_limits()?;
        let total = self.batch.total_work();
        Ok((
            limits.0.saturating_sub(total.source_bytes),
            limits.1.saturating_sub(total.structural_transitions),
        ))
    }

    /// Outer advance checks caller context against current Core selection; inner stages still check the same complete anchor/head pair.
    fn current_authority(&self) -> MarkdownViewportSourceAuthority {
        MarkdownViewportSourceAuthority {
            context_revision: self.context_revision,
            selection: self.selection,
            cancelled: false,
        }
    }
}

fn check_authority(
    source: &MarkdownTableSourceIdentity,
    context_revision: u64,
    selection: MarkdownTextSelection,
    document: &MarkdownDocumentBuffer,
    authority: MarkdownViewportSourceAuthority,
) -> Result<(), MarkdownTableRequestError> {
    if authority.cancelled {
        // Actual cancellation precedes numeric checks; the mutating outer entry fixes that first cause and releases derived state.
        return Err(MarkdownTableRequestError::Cancelled);
    }
    if !source.is_current(document) {
        // Weak allocation identity, generation, and lengths must all match; another equal-content document or a new Undo generation is rejected.
        return Err(MarkdownTableRequestError::SourceSuperseded);
    }
    if context_revision != authority.context_revision
        || selection != authority.selection
        || selection != document.selection()
    {
        // Reversing an equal ordered range changes selection identity; current Core facts/caller context cannot reauthorize old sources.
        return Err(MarkdownTableRequestError::PublicationSuperseded);
    }
    Ok(())
}

fn ordered_selection(selection: MarkdownTextSelection) -> Range<usize> {
    selection.anchor_utf16.min(selection.head_utf16)
        ..selection.anchor_utf16.max(selection.head_utf16)
}

fn publication_metadata() -> usize {
    std::mem::size_of::<VerifiedMarkdownViewportSources>()
        + std::mem::size_of::<SourcePublication>()
        + 2 * std::mem::size_of::<usize>()
}

fn publication_reservation_metadata() -> usize {
    // Reserve for the worst Plain branch upfront; actual work and retained inventory still depend on the real publication variant.
    publication_metadata() + semantic::plain_publication_metadata()
}

fn source_workspace_metadata() -> usize {
    std::mem::size_of::<MarkdownViewportSourceRequest>() + std::mem::size_of::<BlockOriginCursor>()
}

fn record_metadata() -> usize {
    std::mem::size_of::<SourceRecord>()
        + std::mem::size_of::<Rc<SourceRecord>>()
        + 2 * std::mem::size_of::<usize>()
}

fn block_metadata() -> usize {
    std::mem::size_of::<OrdinaryBlockRecord>() + 2 * std::mem::size_of::<usize>()
}

#[cfg(test)]
mod tests;
