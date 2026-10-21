//! Bounded semantics for the source request's closed blocks, ATX headings, code, and rules.
//! Containers consume their original capture/Origin, without another structural frontier.
//!
//! Accept only the parent's actual opaque Leaf/Inline seeds, shared quota, and SourcePublication.
//! Unsupported inline/roles remain unknown. Publish only successful intersections; source bounds or
//! a parsed neighboring block cannot establish Ready semantics for the whole requested window.

use super::semantic::PositionLocator;
use super::*;
use crate::markdown_document_buffer::table_source::MarkdownTableUnderlineRequest;
use crate::markdown_viewport_presentation::parser::{
    HeadingMarkerTail, markdown_selection_intersects,
};
use crate::markdown_viewport_presentation::table_window::MarkdownTableWindowQuota;
use crate::markdown_viewport_presentation::{
    MarkdownPresentationQuery, MarkdownPresentationRun, MarkdownPresentationStyles,
    MarkdownSourceLink, markdown_physical_line_is_active,
};

pub(super) mod scoped;
use scoped::ScopedLeaf;

const CONTROL: usize = 128;
const PAGE_RUNS: usize = 64;
const PAGE_COUNT: usize = 128;
type RunPage = [Option<MarkdownPresentationRun>; PAGE_RUNS];
const LEAF_PAGE_ENTRIES: usize = 16;
const LEAF_PAGE_COUNT: usize = MAX_SOURCE_RECORDS.div_ceil(LEAF_PAGE_ENTRIES);

type LeafPage<T> = [Option<T>; LEAF_PAGE_ENTRIES];
type LeafDirectory<T> = [Option<Box<LeafPage<T>>>; LEAF_PAGE_COUNT];

/// Keep a small single-block window inline; box a multiblock directory so the enum's largest variant
/// does not inflate every publication.
enum LeafStorage<T> {
    Inline(Option<T>),
    Paged(Box<LeafDirectory<T>>),
}

/// Candidates and successful coverage share the original finite capacity. Admit the real directory
/// and first page through the same quota only when a second item arrives.
struct LeafPages<T> {
    storage: LeafStorage<T>,
    len: usize,
}

impl<T> LeafPages<T> {
    fn new() -> Self {
        Self {
            storage: LeafStorage::Inline(None),
            len: 0,
        }
    }
    fn page_count(&self) -> usize {
        self.len.div_ceil(LEAF_PAGE_ENTRIES)
    }
    fn retained_metadata(&self) -> usize {
        match &self.storage {
            LeafStorage::Inline(_) => 0, // The owner's sizeof already includes the inline value; no heap page exists.
            LeafStorage::Paged(_) => {
                std::mem::size_of::<LeafDirectory<T>>()
                    + self.page_count() * std::mem::size_of::<LeafPage<T>>()
            }
        }
    }
    fn get(&self, index: usize) -> Option<&T> {
        if index >= self.len {
            return None; // An unpublished page slot is not a completed source/coverage item.
        }
        match &self.storage {
            LeafStorage::Inline(value) => value.as_ref(),
            LeafStorage::Paged(pages) => {
                pages[index / LEAF_PAGE_ENTRIES].as_ref()?[index % LEAF_PAGE_ENTRIES].as_ref()
            }
        }
    }
    fn iter(&self) -> impl Iterator<Item = &T> {
        let (single, pages) = match &self.storage {
            LeafStorage::Inline(value) => (value.as_ref(), None),
            LeafStorage::Paged(pages) => (None, Some(pages)),
        };
        let count = self.page_count();
        // Both iterators borrow the same mutually exclusive storage, without copying Rc handles or
        // merging intersections belonging to different blocks.
        single
            .into_iter()
            .chain(pages.into_iter().flat_map(move |pages| {
                pages[..count]
                    .iter()
                    .flatten()
                    .flat_map(|page| page.iter().flatten())
            }))
    }
    fn append(
        &mut self,
        value: T,
        quota: &MarkdownTableWindowQuota,
        budget: usize,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        if self.len == MAX_SOURCE_RECORDS {
            return Err(MarkdownTableRequestError::Window(
                MarkdownTableWindowError::SemanticLimit,
            )); // Preserve the original source-directory limit rather than enlarging inventory through another container.
        }
        let page_index = self.len / LEAF_PAGE_ENTRIES;
        let promoting = matches!(&self.storage, LeafStorage::Inline(Some(_)));
        let needs_page = match &self.storage {
            LeafStorage::Inline(_) => promoting,
            LeafStorage::Paged(pages) => pages[page_index].is_none(),
        };
        let bytes = usize::from(promoting) * std::mem::size_of::<LeafDirectory<T>>()
            + usize::from(needs_page) * std::mem::size_of::<LeafPage<T>>();
        let work = SemanticWork {
            // Promotion initializes a real bounded directory/page and moves the sole old value;
            // this work cannot be hidden inside the cheaper second-item write.
            structural_transitions: usize::from(promoting) * (LEAF_PAGE_COUNT + 1)
                + usize::from(needs_page) * LEAF_PAGE_ENTRIES
                + 1,
            metadata_bytes: bytes + (1 + usize::from(promoting)) * std::mem::size_of::<T>(),
            ..SemanticWork::default()
        };
        if work.structural_transitions > budget || work.metadata_bytes > budget {
            return Err(MarkdownTableRequestError::WorkLimit); // Check this actual quantum before admission/allocation.
        }
        quota
            .admit_metadata(bytes)
            .map_err(MarkdownTableRequestError::Window)?;
        if promoting {
            // Move the old value only after all work/capacity admission succeeds; failure leaves the
            // original inline item intact.
            let LeafStorage::Inline(previous) =
                std::mem::replace(&mut self.storage, LeafStorage::Inline(None))
            else {
                unreachable!("提升只来自原单项容器");
            };
            let mut page = Box::new(std::array::from_fn(|_| None));
            page[0] = previous;
            page[1] = Some(value);
            let mut pages: Box<LeafDirectory<T>> = Box::new(std::array::from_fn(|_| None));
            pages[0] = Some(page);
            self.storage = LeafStorage::Paged(pages);
        } else {
            match &mut self.storage {
                LeafStorage::Inline(single) => *single = Some(value),
                LeafStorage::Paged(pages) => {
                    if needs_page {
                        pages[page_index] = Some(Box::new(std::array::from_fn(|_| None)));
                    } // Keep original page boundaries, item limits, and uninitialized-slot restrictions.
                    pages[page_index].as_mut().unwrap()[self.len % LEAF_PAGE_ENTRIES] = Some(value);
                }
            }
        }
        self.len += 1;
        Ok(work)
    }
}

/// Retain the opaque block identity for one real intersection. Do not merge neighboring blocks and
/// erase their distinct sources or unknown gaps.
struct LeafCoverage {
    record: Rc<SourceRecord>,
    utf16: Range<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownRootLeafPendingReason {
    MissingClosedLeaf,
    IncompleteCoverage,
    NonRootScope,
    NeedsGeneralInline,
}

pub(super) enum RootLeafSemanticPublication {
    Pending(MarkdownRootLeafPendingReason),
    Ready(Box<VerifiedMarkdownRootLeafViewport>),
}

type RunDirectory = [Option<Box<RunPage>>; PAGE_COUNT];

/// Store the common single-style window inline; complex windows use the original bounded directory
/// without increasing the maximum run count.
enum RunStorage {
    Inline(Option<MarkdownPresentationRun>),
    Paged(Box<RunDirectory>),
}

/// Merge exactly adjacent equal-style runs in place. Allocate a directory/page for the second
/// independent run rather than constructing 64 empty slots for every short window.
struct RunPages {
    storage: RunStorage,
    len: usize,
}

impl RunPages {
    fn new() -> Self {
        Self {
            storage: RunStorage::Inline(None),
            len: 0,
        }
    }
    fn page_count(&self) -> usize {
        self.len.div_ceil(PAGE_RUNS)
    }
    fn retained_metadata(&self) -> usize {
        match &self.storage {
            RunStorage::Inline(_) => 0, // The inline run belongs to the owner value, with no heap page/directory.
            RunStorage::Paged(_) => {
                std::mem::size_of::<RunDirectory>()
                    + self.page_count() * std::mem::size_of::<RunPage>()
            }
        }
    }
    fn last_mut(&mut self) -> Option<&mut MarkdownPresentationRun> {
        let index = self.len.checked_sub(1)?;
        match &mut self.storage {
            RunStorage::Inline(run) => run.as_mut(),
            RunStorage::Paged(pages) => pages[index / PAGE_RUNS]
                .as_mut()?
                .get_mut(index % PAGE_RUNS)?
                .as_mut(),
        }
    }
    fn append(
        &mut self,
        run: MarkdownPresentationRun,
        quota: &MarkdownTableWindowQuota,
        budget: usize,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        if let Some(previous) = self.last_mut()
            && previous.styles == run.styles
            && previous.utf16_range.end == run.utf16_range.start
        {
            // Exactly adjacent equal-style segments reuse this run without another quota item/page.
            previous.utf16_range.end = run.utf16_range.end;
            return Ok(SemanticWork {
                metadata_bytes: std::mem::size_of::<MarkdownPresentationRun>(),
                ..SemanticWork::default()
            });
        }
        if self.len == PAGE_COUNT * PAGE_RUNS {
            return Err(MarkdownTableRequestError::Window(
                MarkdownTableWindowError::SemanticLimit,
            ));
        }
        let page_index = self.len / PAGE_RUNS;
        let promoting = matches!(&self.storage, RunStorage::Inline(Some(_)));
        let needs_page = match &self.storage {
            RunStorage::Inline(_) => promoting,
            RunStorage::Paged(pages) => pages[page_index].is_none(),
        };
        let allocated = usize::from(promoting) * std::mem::size_of::<RunDirectory>()
            + usize::from(needs_page) * std::mem::size_of::<RunPage>();
        let metadata = allocated
            + (1 + usize::from(promoting)) * std::mem::size_of::<MarkdownPresentationRun>();
        let transitions =
            usize::from(promoting) * (PAGE_COUNT + 1) + usize::from(needs_page) * PAGE_RUNS;
        if metadata > budget || CONTROL + PAGE_RUNS.max(transitions) > budget {
            // Keep the original page-work reserve and include actual promotion/directory/page moves.
            // Check work before capacity admission; never enlarge quota or discard the original
            // inline run to make a multistyle result fit.
            return Err(MarkdownTableRequestError::WorkLimit);
        }
        quota
            .admit_retained(1, allocated)
            .map_err(MarkdownTableRequestError::Window)?;
        if promoting {
            let RunStorage::Inline(previous) =
                std::mem::replace(&mut self.storage, RunStorage::Inline(None))
            else {
                unreachable!("提升只来自原单 run 容器");
            };
            let mut page = Box::new(std::array::from_fn(|_| None));
            page[0] = previous;
            page[1] = Some(run);
            let mut pages: Box<RunDirectory> = Box::new(std::array::from_fn(|_| None));
            pages[0] = Some(page);
            self.storage = RunStorage::Paged(pages);
        } else {
            match &mut self.storage {
                RunStorage::Inline(single) => *single = Some(run),
                RunStorage::Paged(pages) => {
                    if needs_page {
                        pages[page_index] = Some(Box::new(std::array::from_fn(|_| None)));
                    } // Actual page boundaries, including run 65, retain their original index/capacity/quota.
                    pages[page_index].as_mut().unwrap()[self.len % PAGE_RUNS] = Some(run);
                }
            }
        }
        self.len += 1;
        Ok(SemanticWork {
            structural_transitions: transitions,
            metadata_bytes: metadata,
            ..SemanticWork::default()
        })
    }
    fn query(&self, index: usize) -> Option<MarkdownPresentationQuery> {
        if let RunStorage::Inline(run) = &self.storage {
            let run = run.as_ref()?;
            return run
                .utf16_range
                .contains(&index)
                .then(|| MarkdownPresentationQuery {
                    utf16_range: run.utf16_range.clone(),
                    styles: run.styles,
                });
        } // Query the exact half-open inline range; positions outside coverage receive no default style.
        let count = self.page_count();
        let RunStorage::Paged(pages) = &self.storage else {
            unreachable!("内联查询已在前面结束");
        };
        let pages = &pages[..count];
        let page_index = pages.partition_point(|page| {
            let page = page.as_ref().unwrap();
            let last = page.iter().rev().find_map(Option::as_ref).unwrap();
            last.utf16_range.end <= index
        });
        let page = pages.get(page_index)?.as_ref()?;
        let used = (self.len - page_index * PAGE_RUNS).min(PAGE_RUNS);
        let next =
            page[..used].partition_point(|run| run.as_ref().unwrap().utf16_range.end <= index);
        let run = page.get(next)?.as_ref()?;
        run.utf16_range
            .contains(&index)
            .then(|| MarkdownPresentationQuery {
                utf16_range: run.utf16_range.clone(),
                styles: run.styles,
            })
    }
    fn iter(&self) -> impl Iterator<Item = &MarkdownPresentationRun> {
        let (single, pages) = match &self.storage {
            RunStorage::Inline(run) => (run.as_ref(), None),
            RunStorage::Paged(pages) => (None, Some(pages)),
        };
        let count = self.page_count();
        single
            .into_iter()
            .chain(pages.into_iter().flat_map(move |pages| {
                pages[..count]
                    .iter()
                    .flatten()
                    .flat_map(|page| page.iter().flatten())
            }))
    }
}

/// Retain finite run pages, per-closed-block intersections, and weak publication authority. Unproven
/// inline/container regions remain unknown. No body, AST, strong document owner, or Native object is
/// retained; Ready does not mean every position in requested has completed semantics.
pub struct VerifiedMarkdownRootLeafViewport {
    source: MarkdownTableSourceIdentity,
    context_revision: u64,
    selection: MarkdownTextSelection,
    publication: Weak<SourcePublication>,
    requested: Range<usize>,
    coverage: LeafPages<LeafCoverage>,
    runs: RunPages,
    links: Vec<MarkdownSourceLink>,
}

impl VerifiedMarkdownRootLeafViewport {
    /// Borrow the publication's sole actual root-code record for selection invariance, never a
    /// coverage hull as block identity. The SourceRequest retains the Rc; this does not widen
    /// requested, scan the body, or grant container-code authority.
    #[cfg(feature = "paged-line-compose")]
    pub(super) fn root_code_leaf(&self) -> Option<&VerifiedMarkdownOrdinaryLeaf> {
        if self.coverage.len != 1 {
            return None;
        }
        let coverage = self.coverage.get(0)?;
        let SourceRecord::Leaf(leaf) = coverage.record.as_ref() else {
            return None;
        };
        leaf.root_code_body.as_ref()?;
        Some(leaf)
    }

    /// Require the original Plain slot and sole coverage record to share the actual seed, rather
    /// than matching numeric envelopes. Only the parent's synchronous prefix check uses this;
    /// external consumers still need the publication's complete Source/selection authority.
    #[cfg(feature = "paged-line-compose")]
    pub(super) fn covers_plain_block(
        &self,
        block: &Rc<OrdinaryBlockRecord>,
        utf16: &Range<usize>,
    ) -> bool {
        self.coverage.len == 1
            && self.coverage.get(0).is_some_and(|coverage| {
                &coverage.utf16 == utf16
                    && matches!(coverage.record.as_ref(), SourceRecord::Line(line)
                        if line.semantic_block.as_ref().is_some_and(|current| Rc::ptr_eq(current, block)))
            })
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
        if self.coverage.len != 1 {
            return Err(MarkdownTableRequestError::InvalidRequest);
        } // A single-range getter cannot flatten multiple blocks/unknown gaps into a hull.
        Ok(&self.coverage.get(0).unwrap().utf16)
    }
    /// Requested bounds alone grant no semantics to uncovered paragraphs or containers inside them.
    pub fn requested_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<&Range<usize>, MarkdownTableRequestError> {
        self.check(document, authority)?;
        Ok(&self.requested)
    }
    /// Borrow independently proven nonempty intersections in original source order; endpoints do
    /// not authorize filling intervening holes.
    pub fn covered_ranges_for<'a>(
        &'a self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<impl Iterator<Item = &'a Range<usize>> + use<'a>, MarkdownTableRequestError> {
        self.check(document, authority)?;
        Ok(self.coverage.iter().map(|coverage| &coverage.utf16))
    }
    pub fn block_kind_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<LeafBlockKind, MarkdownTableRequestError> {
        self.check(document, authority)?;
        if self.coverage.len != 1 {
            return Err(MarkdownTableRequestError::InvalidRequest);
        } // A window can contain code and a rule; its first kind cannot describe every later block.
        let SourceRecord::Leaf(leaf) = self.coverage.get(0).unwrap().record.as_ref() else {
            // A proven ordinary block has styles but no LeafBlockKind; do not invent a syntax identity.
            return Err(MarkdownTableRequestError::InvalidRequest);
        };
        Ok(leaf.kind())
    }
    /// Return block kinds only inside actual intersections, without a first-block/plain fallback in gaps.
    pub fn block_kind_at_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
        index: usize,
    ) -> Result<Option<LeafBlockKind>, MarkdownTableRequestError> {
        self.check(document, authority)?;
        let Some(coverage) = self
            .coverage
            .iter()
            .find(|part| part.utf16.contains(&index))
        else {
            return Ok(None); // This bounded lookup covers at most 256 source intersections and reads no body.
        };
        let SourceRecord::Leaf(leaf) = coverage.record.as_ref() else {
            return Ok(None); // Actual runs authorize ordinary closed blocks; they have no fabricated Leaf kind.
        };
        Ok(Some(leaf.kind()))
    }
    pub fn query_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
        index: usize,
    ) -> Result<Option<MarkdownPresentationQuery>, MarkdownTableRequestError> {
        self.check(document, authority)?;
        let Some(coverage) = self
            .coverage
            .iter()
            .find(|part| part.utf16.contains(&index))
        else {
            return Ok(None); // Requested holes remain unknown, without default style/hiding/drawing authority.
        };
        Ok(self.runs.query(index).map(|mut query| {
            // Adjacent equal-style runs may share a page record, but this query authorizes only
            // the actual leaf intersection.
            query.utf16_range.start = query.utf16_range.start.max(coverage.utf16.start);
            query.utf16_range.end = query.utf16_range.end.min(coverage.utf16.end);
            query
        }))
    }
    pub fn runs_for<'a>(
        &'a self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<
        impl Iterator<Item = &'a MarkdownPresentationRun> + use<'a>,
        MarkdownTableRequestError,
    > {
        self.check(document, authority)?;
        Ok(self.runs.iter())
    }
    pub(super) fn retained_metadata(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.coverage.retained_metadata()
            + self.runs.retained_metadata()
            + self.links.capacity() * std::mem::size_of::<MarkdownSourceLink>()
            + self
                .links
                .iter()
                .map(|link| link.destination.retained_metadata())
                .sum::<usize>()
    }
    /// A finite parser's proven complete label can provide a positive link outside style coverage.
    /// Without that positive fact, only None within published coverage proves absence; unknown
    /// regions retain ContextPending rather than gaining styles or negative link authority.
    pub fn link_at_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
        index: usize,
    ) -> Result<Option<&MarkdownSourceLink>, MarkdownTableRequestError> {
        self.check(document, authority)?;
        if let Some(link) = self
            .links
            .iter()
            .find(|link| link.label_utf16_range.contains(&index))
        {
            // Complete finite events from the same-source closed block proved/admitted the label
            // and decoded target; the current small viewport cannot truncate those positive facts.
            return Ok(Some(link));
        }
        if self.query_for(document, authority, index)?.is_none() {
            return Err(MarkdownTableRequestError::Window(
                MarkdownTableWindowError::ContextPending,
            ));
        }
        Ok(None)
    }
    pub(super) fn run_count(&self) -> usize {
        self.runs.len
    }
}

#[derive(Clone, Copy)]
enum Phase {
    Bounds,
    ResumeUnderline,
    AwaitSource,
    NextLeaf,
    Scope,
    HeadingMarkers,
    Heading,
    Coverage,
    Boundary,
    Coordinates,
    Underline,
    Build,
    AtxInline,
    Ordinary,
    OrdinaryRuns,
    DiscardOrdinary,
    FinishLeaf,
    Done,
    Published,
}

/// Leaf continuation for one source publication. The parent's account_source_work records actual
/// work in the original cumulative fuel; quota must share that batch's Rc, without fresh allowance
/// or another Global.
pub(super) struct RootLeafSemanticJob {
    source: MarkdownTableSourceIdentity,
    context_revision: u64,
    selection: MarkdownTextSelection,
    requested: Range<usize>,
    positions: [PositionLocator; 4],
    position_index: usize,
    byte_coverage: Option<Range<usize>>,
    // Reuse only the same-source parent's completed selection endpoints; None requires actual cold lookup.
    selection_bytes: Option<Range<usize>>,
    candidates: LeafPages<Rc<SourceRecord>>,
    candidate_index: usize,
    coverage: LeafPages<LeafCoverage>,
    record: Option<Rc<SourceRecord>>,
    active_byte_coverage: Option<Range<usize>>,
    active_coverage: Option<Range<usize>>,
    saw_intersection: bool,
    boundary: Option<FenceBoundaryProof>,
    scoped: Option<Box<ScopedLeaf>>,
    ordinary_run: Option<MarkdownPresentationRun>,
    heading_next: Option<usize>,
    heading_tail: Option<AtxMarkerTail>,
    raw_markers: [Option<Range<usize>>; 2],
    projection: Option<Coordinates>,
    markers: Option<[Range<usize>; 2]>,
    underline: Option<MarkdownTableUnderlineRequest>,
    underline_index: usize,
    // Ordinary blocks borrow the parent's sole admitted semantic slot; retain only the run handoff index.
    ordinary_run_index: usize,
    run_next: usize,
    runs: RunPages,
    links: Vec<MarkdownSourceLink>,
    quota: MarkdownTableWindowQuota,
    pending: Option<MarkdownRootLeafPendingReason>,
    failed: Option<MarkdownTableRequestError>,
    phase: Phase,
}

impl RootLeafSemanticJob {
    /// Charge actual fixed-continuation disposal, such as reverse repositioning; Ready/Pending
    /// handoff alone does not report a fictitious drop.
    pub(super) fn completed_owner_drop_control() -> usize {
        // Publication already returned candidates/moved output. Keep the original fixed-directory
        // cleanup upper-bound reserve; inline slots imply no allocated/traversed pages and grant
        // no additional control fuel after the storage optimization.
        CONTROL + PAGE_COUNT + 2 * LEAF_PAGE_COUNT
    }

    pub(super) fn construction_work() -> SemanticWork {
        SemanticWork {
            // Keep the bounded-directory conservative control reserve; metadata records only the
            // actual small owner allocated now.
            structural_transitions: CONTROL + PAGE_COUNT + 2 * LEAF_PAGE_COUNT,
            metadata_bytes: std::mem::size_of::<Self>(),
            ..SemanticWork::default()
        }
    }
    pub(super) fn new(
        source: MarkdownTableSourceIdentity,
        context_revision: u64,
        selection: MarkdownTextSelection,
        requested: Range<usize>,
        selection_bytes: Option<Range<usize>>,
        underline: Option<MarkdownTableUnderlineRequest>,
        quota: MarkdownTableWindowQuota,
    ) -> Result<Self, MarkdownTableRequestError> {
        if requested.start >= requested.end || requested.len() > 64 * 1_024 {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        // Admit the fixed job and future publication header jointly; no body/run/underline pages
        // are allocated here.
        quota
            .admit_metadata(
                std::mem::size_of::<Self>()
                    + std::mem::size_of::<VerifiedMarkdownRootLeafViewport>(),
            )
            .map_err(MarkdownTableRequestError::Window)?;
        let ordered = ordered_selection(selection);
        Ok(Self {
            source,
            context_revision,
            selection,
            positions: [
                PositionLocator::new(requested.start, false),
                PositionLocator::new(requested.end, true),
                PositionLocator::new(ordered.start, false),
                PositionLocator::new(ordered.end, true),
            ],
            position_index: 0,
            run_next: requested.start,
            requested,
            byte_coverage: None,
            selection_bytes,
            candidates: LeafPages::new(),
            candidate_index: 0,
            coverage: LeafPages::new(),
            record: None,
            active_byte_coverage: None,
            active_coverage: None,
            saw_intersection: false,
            boundary: None,
            scoped: None,
            ordinary_run: None,
            heading_next: None,
            heading_tail: None,
            raw_markers: [None, None],
            projection: None,
            markers: None,
            underline,
            underline_index: 0,
            ordinary_run_index: 0,
            runs: RunPages::new(),
            links: Vec::new(),
            quota,
            pending: None,
            failed: None,
            phase: Phase::Bounds,
        })
    }

    /// Reuse the parent's sole slot after Ready moved all output. Bounds must prove the new window's
    /// own byte coverage; retain two small lookup frontiers and immutable Source/full selection,
    /// without an old publication or future semantic result.
    pub(super) fn continue_forward(
        &mut self,
        requested: Range<usize>,
        underline: Option<MarkdownTableUnderlineRequest>,
        quota: MarkdownTableWindowQuota,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        if !matches!(self.phase, Phase::Published)
            || requested.start < self.requested.start
            || requested.end < self.requested.end
            || requested.is_empty()
            || requested.len() > 64 * 1_024
        {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        // Old Ready still occupies joint inventory; admit this fixed job to the latest same-batch
        // quota rather than reusing the previous admission.
        quota
            .admit_metadata(
                std::mem::size_of::<Self>()
                    + std::mem::size_of::<VerifiedMarkdownRootLeafViewport>(),
            )
            .map_err(MarkdownTableRequestError::Window)?;
        let first = self.positions[0].retarget_forward(requested.start, false);
        let last = self.positions[1].retarget_forward(requested.end, true);
        self.run_next = requested.start;
        self.requested = requested;
        self.position_index = 0;
        self.byte_coverage = None;
        self.candidate_index = 0;
        self.saw_intersection = false;
        self.underline = underline;
        self.underline_index = 0;
        self.ordinary_run_index = 0;
        self.quota = quota;
        self.pending = None;
        self.phase = Phase::Bounds;
        // Publication moved/cleared the directories; only fixed retarget work occurs, with no new
        // page arrays/Box. Selection endpoints retain original full-selection authority, while both
        // viewport locators must complete again; old styles grant no new coverage.
        Ok(SemanticWork {
            structural_transitions: CONTROL
                + first.structural_transitions
                + last.structural_transitions,
            metadata_bytes: first.metadata_bytes + last.metadata_bytes,
            ..SemanticWork::default()
        })
    }
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
        self.failed = Some(error);
        self.candidates = LeafPages::new();
        self.coverage = LeafPages::new();
        self.record = None;
        self.active_byte_coverage = None;
        self.active_coverage = None;
        self.boundary = None;
        self.scoped = None;
        self.ordinary_run = None;
        self.heading_next = None;
        self.heading_tail = None;
        self.projection = None;
        self.underline = None;
        self.runs = RunPages::new(); // Return derived pages without rewriting old external publications.
        self.links = Vec::new(); // Return this publication's untransferred URL bytes/directory on failure.
    }
    pub(super) fn cancel(&mut self) {
        if self.failed.is_none() {
            self.fail(MarkdownTableRequestError::Cancelled);
        }
    }
    pub(super) fn preparing(&self) -> bool {
        matches!(self.phase, Phase::Bounds | Phase::ResumeUnderline)
    }
    pub(super) fn done(&self) -> bool {
        matches!(self.phase, Phase::Done)
    }

    /// Return selection boundaries only after actual original endpoint lookup; the parent retains
    /// the same Source and complete selection.
    pub(super) fn verified_selection_bytes(&self) -> Option<Range<usize>> {
        self.selection_bytes.clone()
    }

    /// Publication moves style inventory only; return the unique scanner to the original request so
    /// later windows consume its actual UTF-16/tag frontier.
    pub(super) fn return_underline(&mut self) -> Option<MarkdownTableUnderlineRequest> {
        self.underline.take()
    }

    /// Preserve the next line's same seed when the original producer consumed ahead across blank
    /// space. Old blocks behind coverage still drop immediately; this ticket grants no extra
    /// semantics to the current viewport.
    pub(super) fn retains_inline_seed(&self, physical: &Range<usize>) -> bool {
        self.byte_coverage
            .as_ref()
            .is_some_and(|coverage| coverage.start < physical.end)
    }

    /// Admit only the parent's actual source Rc records that intersect this page.
    pub(super) fn offer_record(
        &mut self,
        record: &Rc<SourceRecord>,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        if !matches!(self.phase, Phase::AwaitSource) {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        let physical = record.utf16_range();
        let mut work = SemanticWork {
            structural_transitions: CONTROL,
            ..SemanticWork::default()
        };
        if physical.start < self.requested.end && self.requested.start < physical.end {
            self.saw_intersection = true;
            let source_start = match record.as_ref() {
                SourceRecord::Leaf(leaf) => leaf.source_range().start,
                SourceRecord::Line(line) => {
                    let Some(block) = line.semantic_block.as_ref() else {
                        return Ok(work); // SourcesOnly lines carry source measurements, without an Inline seed.
                    };
                    block.seed.source_range().start
                }
            };
            if let Some(previous) = self.candidates.get(self.candidates.len.saturating_sub(1)) {
                let previous_start = match previous.as_ref() {
                    SourceRecord::Leaf(leaf) => {
                        if Rc::ptr_eq(previous, record) {
                            return Ok(work); // Reencountering the same opaque Leaf consumes no second candidate.
                        }
                        leaf.source_range().start
                    }
                    SourceRecord::Line(line) => {
                        let block = line.semantic_block.as_ref().unwrap();
                        if matches!(record.as_ref(), SourceRecord::Line(current)
                            if current.semantic_block.as_ref().is_some_and(|current| Rc::ptr_eq(current, block)))
                        {
                            return Ok(work); // Prove a multiline closed block once, without per-line duplicate coverage.
                        }
                        if Rc::ptr_eq(previous, record) {
                            return Ok(work); // Replayed container lines consume no duplicate candidate/semantic inventory.
                        }
                        block.seed.source_range().start
                    }
                };
                if previous_start >= source_start {
                    return Err(MarkdownTableRequestError::InvalidRequest);
                } // Keep the same Global's actual source order; sorting/numeric equality cannot repair identity.
            }
            let measured =
                self.candidates
                    .append(Rc::clone(record), &self.quota, 4096 - CONTROL)?;
            work.structural_transitions += measured.structural_transitions;
            work.metadata_bytes = measured.metadata_bytes;
        }
        Ok(work)
    }

    /// Share this entry across Bounds and later leaf phases; the parent advances post-Bounds work
    /// only after source readiness. Recheck Source/selection/cancellation on every call, retaining
    /// actual revocation as the first failure rather than duplicating parent position flags.
    pub(super) fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
        budget: usize,
        limits: (usize, usize),
        ordinary: &mut Option<PlainSemanticJob>,
        global: Option<&mut GlobalFrontier>,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let result = self.check(document, authority).and_then(|()| {
            if !(4096..=65536).contains(&budget) || matches!(self.phase, Phase::Published) {
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            self.advance_inner(document, authority, budget, limits, ordinary, global)
        });
        if let Err(error) = result {
            *ordinary = None; // The same failed request must retain no old cursor or untransferred underline in its parent slot.
            self.fail(error);
        }
        result
    }
    fn advance_inner(
        &mut self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
        budget: usize,
        limits: (usize, usize),
        ordinary: &mut Option<PlainSemanticJob>,
        global: Option<&mut GlobalFrontier>,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let mut work = SemanticWork {
            structural_transitions: CONTROL,
            ..SemanticWork::default()
        };
        match self.phase {
            Phase::Bounds => work = self.advance_bounds(document, authority, budget)?,
            Phase::ResumeUnderline => {
                // All required endpoints are proven. Retarget this scanner in its own quantum;
                // if it cannot continue, return the old owner and let Underline use the original
                // cold factory to authenticate from the root.
                let mut underline = self.underline.take().expect("续批持原唯一scanner");
                if underline.continue_source(
                    document,
                    self.byte_coverage.as_ref().unwrap().clone(),
                    self.requested.clone(),
                    self.quota.clone(),
                )? {
                    self.underline = Some(underline);
                }
                work.metadata_bytes = 2 * std::mem::size_of::<Range<usize>>();
                self.phase = Phase::AwaitSource;
            }
            Phase::AwaitSource => {
                self.phase = Phase::NextLeaf; // Source completion ends collection; leaf proof starts next quantum.
            }
            Phase::NextLeaf => {
                if self.candidate_index == self.candidates.len
                    && let Some(underline) = self.underline.as_mut()
                {
                    if underline
                        .ready_pairs_for(document)
                        .map_err(MarkdownTableRequestError::Window)?
                        .is_none()
                    {
                        // An empty LF has no leaf consumer, but ResumeUnderline opened its original
                        // scan transaction. Complete it on the same Global across quanta; returning
                        // an unfinished owner would force the next window to rescan all prior text.
                        return self.advance_underline(document, global, budget);
                    }
                    // Even empty coverage ends the original inline-file transaction; preserve any
                    // close failure through the request's existing first-error path.
                    underline.close_inline_stack()?;
                }
                work = self.begin_next_leaf(document, limits, ordinary)?;
            }
            Phase::Scope => {
                let scoped = self.scoped.as_mut().unwrap();
                let (measured, done) = scoped.advance(document, &self.quota, budget - CONTROL)?;
                work = measured;
                work.structural_transitions += CONTROL;
                if done {
                    if scoped.unsupported() {
                        self.pending
                            .get_or_insert(MarkdownRootLeafPendingReason::NonRootScope);
                        self.clear_active_leaf();
                        self.phase = Phase::NextLeaf;
                    } else {
                        let SourceRecord::Leaf(leaf) = self.record.as_ref().unwrap().as_ref()
                        else {
                            unreachable!("Scope 只消费真实 Leaf");
                        };
                        self.phase = match leaf.kind() {
                            LeafBlockKind::AtxHeading(_) if self.heading_tail.is_some() => {
                                Phase::HeadingMarkers
                            }
                            LeafBlockKind::AtxHeading(_) => Phase::Coverage,
                            LeafBlockKind::FencedCode => Phase::Boundary,
                            LeafBlockKind::IndentedCode
                            | LeafBlockKind::Html
                            | LeafBlockKind::Metadata(_) => Phase::Coverage,
                            _ => return Err(MarkdownTableRequestError::InvalidRequest),
                        };
                    }
                } // Heading/code work waits for original capture and this window's actual line prefixes.
            }
            Phase::HeadingMarkers => {
                let tail = self.heading_tail.as_mut().unwrap();
                work = tail.advance(document, budget - CONTROL)?;
                work.structural_transitions += CONTROL;
                if tail.done() {
                    self.raw_markers[1] = tail.marker();
                    self.heading_tail = None;
                    self.phase = Phase::Heading;
                } // Complete the shared presentation tail before Plain body work; firstpass closure stays authoritative.
            }
            Phase::Heading => work = self.advance_heading(document, budget)?,
            Phase::Coverage => {
                let bytes = self.active_byte_coverage.as_ref().unwrap();
                if let Some(projection) = self.projection.as_mut() {
                    let (measured, values) = projection.advance(document, budget - CONTROL)?;
                    work = measured;
                    work.structural_transitions += CONTROL;
                    if let Some(values) = values {
                        let start = values[0].max(self.requested.start);
                        let end = values[1].min(self.requested.end);
                        if start >= end {
                            return Err(MarkdownTableRequestError::InvalidRequest);
                        } // A nonempty byte intersection needs nonempty scalar-aligned UTF-16 coverage.
                        self.active_coverage = Some(start..end);
                        self.run_next = start;
                        self.projection = None;
                        self.phase = match self.record.as_ref().unwrap().as_ref() {
                            SourceRecord::Leaf(leaf)
                                if matches!(leaf.kind(), LeafBlockKind::AtxHeading(_))
                                    && self.heading_next.is_some() =>
                            {
                                Phase::AtxInline
                            }
                            SourceRecord::Leaf(_) => Phase::Coordinates,
                            SourceRecord::Line(_) => Phase::Underline,
                        }; // Reuse measured ordinary coverage; its actual Plain cursor must still prove the styles.
                    }
                } else {
                    // Actual source bounds can differ from physical/consumed bounds; a neighboring
                    // line's prefix must not enter this leaf's coverage.
                    let (projection, measured) = Coordinates::new(
                        document,
                        [bytes.start, bytes.end, bytes.end, bytes.end],
                        budget - CONTROL,
                    )?;
                    self.projection = Some(projection);
                    work = measured;
                    work.structural_transitions += CONTROL;
                }
            }
            Phase::Boundary => {
                let boundary = self.boundary.as_mut().unwrap();
                work = boundary.advance(document)?;
                work.structural_transitions += CONTROL;
                if boundary.done {
                    self.raw_markers = [Some(boundary.opening.clone()), boundary.closing.clone()];
                    self.phase = Phase::Coverage;
                }
            }
            Phase::Coordinates => {
                if self.projection.is_none() {
                    let coverage = self.active_byte_coverage.as_ref().unwrap();
                    let empty_start = match self.record.as_ref().unwrap().as_ref() {
                        SourceRecord::Leaf(leaf) if leaf.kind() == LeafBlockKind::IndentedCode => {
                            leaf.source_range()
                                .start
                                .clamp(coverage.start, coverage.end)
                        }
                        _ => coverage.start,
                    }; // Indented code has no fence; keep its real body start as the prefix style boundary.
                    let first =
                        self.raw_markers[0]
                            .as_ref()
                            .map_or(empty_start..empty_start, |range| {
                                range.start.clamp(coverage.start, coverage.end)
                                    ..range.end.clamp(coverage.start, coverage.end)
                            });
                    let last =
                        self.raw_markers[1]
                            .as_ref()
                            .map_or(coverage.end..coverage.end, |range| {
                                range.start.clamp(coverage.start, coverage.end)
                                    ..range.end.clamp(coverage.start, coverage.end)
                            });
                    let (projection, measured) = Coordinates::new(
                        document,
                        [first.start, first.end, last.start, last.end],
                        budget - CONTROL,
                    )?;
                    self.projection = Some(projection);
                    work = measured;
                    work.structural_transitions += CONTROL;
                } else {
                    let (measured, values) = self
                        .projection
                        .as_mut()
                        .unwrap()
                        .advance(document, budget - CONTROL)?;
                    work = measured;
                    work.structural_transitions += CONTROL;
                    if let Some(values) = values {
                        self.markers = Some([values[0]..values[1], values[2]..values[3]]);
                        self.projection = None;
                        self.phase = match self.record.as_ref().unwrap().as_ref() {
                            SourceRecord::Leaf(leaf)
                                if matches!(
                                    leaf.kind(),
                                    LeafBlockKind::FencedCode | LeafBlockKind::IndentedCode
                                ) =>
                            {
                                Phase::Build
                            }
                            _ => Phase::Underline, // Non-code leaves still require actual shared full-source pairing.
                        }; // Code stays literal without creating/advancing HTML pairing; retain its shared owner for later blocks.
                    }
                }
            }
            Phase::Underline => {
                if let Some(underline) = self.underline.as_mut() {
                    if matches!(
                        self.record.as_ref().unwrap().as_ref(),
                        SourceRecord::Line(_)
                    ) {
                        // Give the closed block its original pairing owner before collecting finite
                        // references, avoiding a cycle where references before code block pairing
                        // while pairing prevents preparing those queries.
                        work = self.begin_ordinary(ordinary, budget)?;
                    } else if underline
                        .ready_pairs_for(document)
                        .map_err(MarkdownTableRequestError::Window)?
                        .is_some()
                    {
                        // Only non-code blocks consume full-source pairing; literal code never
                        // interprets HTML or changes this shared cursor.
                        self.phase = Phase::Build;
                    } else {
                        // Advance one original bounded frame, without rebuilding context or looping to Ready.
                        work = self.advance_underline(document, global, budget)?;
                    }
                } else {
                    // Share table/Plain's unique underline workspace, admitting capacity before
                    // constructing context or pair inventory.
                    self.quota
                        .reserve_underline_workspace()
                        .map_err(MarkdownTableRequestError::Window)?;
                    self.underline = Some(
                        document
                            .table_source_underline_request(
                                self.byte_coverage.as_ref().unwrap().clone(),
                                self.quota.clone(),
                            )
                            .map_err(MarkdownTableRequestError::Window)?,
                    );
                }
            }
            Phase::Build => work = self.build_one(document, budget)?,
            Phase::AtxInline => work = self.begin_atx_inline(ordinary, budget)?,
            Phase::Ordinary => {
                let job = ordinary.as_mut().unwrap();
                let (measured, _) = match self.record.as_ref().unwrap().as_ref() {
                    SourceRecord::Leaf(leaf) => job.advance_atx_after_sources(
                        document,
                        &leaf.block,
                        budget - CONTROL,
                        limits,
                        global,
                    )?,
                    SourceRecord::Line(_) => {
                        job.advance_after_sources(document, budget - CONTROL, limits, global)?
                    }
                };
                work = measured;
                work.structural_transitions += CONTROL;
                if job.done() {
                    if let Some(reason) = job.pending_reason() {
                        self.pending.get_or_insert(match reason {
                            MarkdownOrdinaryPendingReason::MissingClosedBlock => {
                                MarkdownRootLeafPendingReason::MissingClosedLeaf
                            }
                            MarkdownOrdinaryPendingReason::IncompleteCoverage => {
                                MarkdownRootLeafPendingReason::IncompleteCoverage
                            }
                            MarkdownOrdinaryPendingReason::NeedsGeneralInline => {
                                MarkdownRootLeafPendingReason::NeedsGeneralInline
                            }
                            MarkdownOrdinaryPendingReason::NonRootScope => {
                                MarkdownRootLeafPendingReason::NonRootScope
                            }
                        });
                        self.phase = Phase::DiscardOrdinary; // Dispose next quantum, preserving the reference scanner's minimum work.
                    } else {
                        self.phase = Phase::OrdinaryRuns;
                    } // Scratch runs are not output inventory yet; admit each jointly in subsequent quanta.
                }
            }
            Phase::OrdinaryRuns => {
                let job = ordinary.as_mut().unwrap();
                if self.ordinary_run.is_none() {
                    self.ordinary_run = job.take_proven_run(self.ordinary_run_index)?;
                }
                if let Some(run) = self.ordinary_run.as_mut() {
                    let query = self
                        .scoped
                        .as_ref()
                        .map(|scoped| scoped.query(run.utf16_range.start, run.utf16_range.end));
                    let end = query
                        .as_ref()
                        .map_or(run.utf16_range.end, |query| query.utf16_range.end);
                    let styles = query.map_or(run.styles, |query| run.styles.union(query.styles));
                    let measured = self.runs.append(
                        MarkdownPresentationRun {
                            utf16_range: run.utf16_range.start..end,
                            styles,
                        },
                        &self.quota,
                        budget - CONTROL,
                    )?;
                    run.utf16_range.start = end;
                    if run.utf16_range.is_empty() {
                        self.ordinary_run = None;
                        self.ordinary_run_index += 1;
                    }
                    work.structural_transitions += measured.structural_transitions;
                    work.metadata_bytes = measured.metadata_bytes;
                } else if let Some(link) = job.take_proven_link()? {
                    // Move URL ownership without copying bytes. Admit the new directory while it
                    // coexists with the original job.
                    self.quota
                        .admit_metadata(std::mem::size_of::<MarkdownSourceLink>())
                        .map_err(MarkdownTableRequestError::Window)?;
                    self.links.reserve_exact(1);
                    self.links.push(link);
                    work.metadata_bytes = std::mem::size_of::<MarkdownSourceLink>();
                } else {
                    self.underline = job.return_underline();
                    // Keep a completed plain prefix in the parent's sole slot, reusable only with
                    // the same seed. Complex semantics retain no historical workspace; after its
                    // pairing owner returns, dispose the actual scratch in this quantum.
                    if !job.retains_plain_prefix() {
                        *ordinary = None;
                        work.structural_transitions += PlainSemanticJob::completed_drop_control();
                    }
                    self.phase = Phase::FinishLeaf;
                }
            }
            Phase::DiscardOrdinary => {
                self.underline = ordinary.as_mut().unwrap().return_underline();
                // General/Pending ends this ordinary consumer without visiting FinishLeaf. Close
                // the same external stack before discarding scratch and preserve real close errors.
                if let Some(underline) = self.underline.as_mut() {
                    underline.close_inline_stack()?;
                }
                *ordinary = None;
                self.clear_active_leaf();
                self.phase = Phase::NextLeaf;
                work.structural_transitions += PlainSemanticJob::completed_drop_control();
            }
            Phase::FinishLeaf => {
                if let Some(underline) = self.underline.as_mut() {
                    underline.close_inline_stack()?;
                }
                let record = Rc::clone(self.record.as_ref().unwrap());
                let utf16 = self.active_coverage.as_ref().unwrap().clone();
                if self
                    .coverage
                    .get(self.coverage.len.saturating_sub(1))
                    .is_some_and(|previous| previous.utf16.end > utf16.start)
                {
                    return Err(MarkdownTableRequestError::InvalidRequest);
                } // Successful intersections cannot overlap; clipping, sorting, or a hull cannot repair contradictory sources.
                let measured = self.coverage.append(
                    LeafCoverage { record, utf16 },
                    &self.quota,
                    budget - CONTROL,
                )?;
                work.structural_transitions += measured.structural_transitions;
                work.metadata_bytes = measured.metadata_bytes;
                self.clear_active_leaf();
                self.phase = Phase::NextLeaf; // Admit coverage only after all runs succeed; unsupported holes retain no run.
            }
            Phase::Done => {} // Explicit publication moves results once; repeated advance allocates no second Ready.
            Phase::Published => return Err(MarkdownTableRequestError::InvalidRequest),
        }
        Ok(work)
    }

    /// Ordinary leaves and empty-coverage completion share the original transaction, advancing once
    /// per call and reporting all actual work to the parent batch.
    fn advance_underline(
        &mut self,
        document: &MarkdownDocumentBuffer,
        global: Option<&mut GlobalFrontier>,
        budget: usize,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let measured = self.underline.as_mut().unwrap().advance(
            document,
            global.ok_or(MarkdownTableRequestError::InvalidRequest)?,
            budget - CONTROL,
        )?;
        Ok(SemanticWork {
            source_bytes: measured.source_bytes + measured.global_source_bytes,
            utf8_validation_bytes: measured.utf8_validation_bytes,
            source_tree_steps: measured.source_tree_steps + measured.global_source_tree_steps,
            structural_transitions: CONTROL + measured.structural_transitions,
            copied_source_bytes: measured.copied_source_bytes,
            metadata_bytes: measured.metadata_bytes,
            normalizer_input_bytes: measured.normalizer_input_bytes,
            parser_input_bytes: measured.parser_input_bytes,
            allocated_source_capacity: measured.allocated_source_capacity,
        })
    }

    /// Share one original 4096 quantum across cold-window endpoint work, without scheduling each
    /// small tree/scalar step separately. Only coalesce existing Bounds steps; never advance Global,
    /// rebuild locators, or borrow the next quantum's budget.
    fn advance_bounds(
        &mut self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
        budget: usize,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let mut work = SemanticWork::default();
        let mut remaining = budget;
        // Source/context/full selection remain those of the original request. Reuse only selection
        // bytes authenticated by its prior real publication, avoiding repeated far-prefix decoding
        // per tiny window. Cold work still locates all four endpoints; warm work locates both current
        // viewport endpoints with the original locators and budget.
        let position_limit = if self.selection_bytes.is_some() { 2 } else { 4 };
        while self.position_index < position_limit {
            let locator = &mut self.positions[self.position_index];
            let may_reuse = self.position_index != 0 && locator.needs_location();
            let reuse_reservation = if may_reuse {
                PositionLocator::PREFIX_REUSE_CONTROL + PositionLocator::PREFIX_REUSE_METADATA
            } else {
                0
            };
            if remaining < CONTROL + locator.minimum_budget(document) + reuse_reservation {
                break; // Resume this locator next quantum; low balance cannot create an invalid child budget or completed endpoint.
            }
            self.check(document, authority)?;
            let mut measured = self.positions[self.position_index]
                .advance(document, remaining - CONTROL - reuse_reservation)?;
            self.check(document, authority)?; // Recheck original Source/cancellation/full selection after each actual lease return.
            if may_reuse {
                // Reuse a prior endpoint only after independently locating this target's same
                // piece/ordinal, without linearly scanning the body between distant selections.
                let (previous, following) = self.positions.split_at_mut(self.position_index);
                let reused =
                    following[0].reuse_completed_prefix(&previous[self.position_index - 1]);
                measured.structural_transitions += PositionLocator::PREFIX_REUSE_CONTROL;
                if reused {
                    measured.metadata_bytes += PositionLocator::PREFIX_REUSE_METADATA;
                } // Charge no metadata for an absent scalar handoff; unused reserve stays in this quantum.
            }
            // Match Coordinates accounting: transitions already include the tree path, and decoding
            // already accounts UTF-8 validation.
            let cost = measured.source_bytes
                + measured.structural_transitions
                + measured.metadata_bytes
                + CONTROL;
            remaining -= cost;
            work.source_bytes += measured.source_bytes;
            work.utf8_validation_bytes += measured.utf8_validation_bytes;
            work.source_tree_steps += measured.source_tree_steps;
            work.structural_transitions += measured.structural_transitions + CONTROL;
            work.metadata_bytes += measured.metadata_bytes;
            if self.positions[self.position_index].result().is_some() {
                self.position_index += 1; // Move to the next endpoint only after the original locator yields its byte boundary.
            }
            // Every step pays PLAIN_CONTROL and performs lookup/read/result work; the remaining
            // balance strictly decreases rather than permitting an empty spin.
        }
        if self.position_index == position_limit {
            self.byte_coverage =
                Some(self.positions[0].result().unwrap()..self.positions[1].result().unwrap());
            if self.selection_bytes.is_none() {
                self.selection_bytes =
                    Some(self.positions[2].result().unwrap()..self.positions[3].result().unwrap());
            }
            self.phase = if self.underline.is_some() {
                Phase::ResumeUnderline
            } else {
                Phase::AwaitSource
            }; // Endpoints are complete; retargeting the original scanner still owns a later quantum.
        }
        Ok(work)
    }

    /// Consume one same-source candidate per quantum, without skipping every unknown block in a
    /// loop or reclassifying syntax from visible body text.
    fn begin_next_leaf(
        &mut self,
        document: &MarkdownDocumentBuffer,
        limits: (usize, usize),
        ordinary: &mut Option<PlainSemanticJob>,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let mut work = SemanticWork {
            structural_transitions: CONTROL,
            ..SemanticWork::default()
        };
        let Some(record) = self.candidates.get(self.candidate_index).map(Rc::clone) else {
            if self.coverage.len == 0 && self.pending.is_none() {
                self.pending = Some(if self.saw_intersection {
                    MarkdownRootLeafPendingReason::IncompleteCoverage
                } else {
                    MarkdownRootLeafPendingReason::MissingClosedLeaf
                });
            } // Only real successful segments can become Ready; an empty directory remains explicitly Pending.
            self.phase = Phase::Done;
            return Ok(work);
        };
        if ordinary.as_ref().is_some_and(|job| match record.as_ref() {
            SourceRecord::Line(line) => line
                .semantic_block
                .as_ref()
                .is_none_or(|block| !job.retains_plain_block(block)),
            SourceRecord::Leaf(_) => true,
        }) {
            // A new block cannot inherit the prior plain prefix. Dispose only that old owner now,
            // leaving this candidate in place for the next quantum rather than combining disposal
            // and construction.
            *ordinary = None;
            work.structural_transitions += PlainSemanticJob::completed_drop_control();
            return Ok(work);
        }
        self.candidate_index += 1;
        let SourceRecord::Leaf(leaf) = record.as_ref() else {
            let SourceRecord::Line(line) = record.as_ref() else {
                unreachable!("来源只有 Leaf 或 Line");
            };
            let block = line.semantic_block.as_ref().unwrap();
            let physical = block.seed.physical_range();
            let requested = self.byte_coverage.as_ref().unwrap();
            let intersection = physical.start.max(requested.start)..physical.end.min(requested.end);
            if !intersection.is_empty() {
                let complete_window = intersection == *requested;
                self.active_byte_coverage = Some(intersection);
                self.record = Some(record);
                if complete_window {
                    // Bounds proved the original request's floor/ceil bytes. When the actual block
                    // contains that envelope, decoding and clipping to requested yields the same
                    // UTF-16 interval, including surrogate-interior endpoints; scalar alignment
                    // cannot enlarge published coverage.
                    self.active_coverage = Some(self.requested.clone());
                    self.run_next = self.requested.start;
                    self.phase = Phase::Underline;
                    work.metadata_bytes += std::mem::size_of::<Range<usize>>()
                        + std::mem::size_of::<Option<Range<usize>>>()
                        + std::mem::size_of::<usize>();
                } else {
                    // A partial block intersection still requires independent UTF-16 endpoint lookup.
                    self.phase = Phase::Coverage;
                }
            } // The original closed Inline seed defines physical bounds; neighboring paragraphs cannot be folded in.
            return Ok(work);
        };
        if leaf.block.source_revision() != document.generation()
            || leaf.block.source_end() != document.byte_len()
        {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        if leaf.block.options()
            != crate::markdown_viewport_presentation::parser::markdown_presentation_options(true)
        {
            return Err(MarkdownTableRequestError::InvalidRequest);
        } // Opaque facts produced with different parser options grant no current-product semantics.
        let scoped = leaf.block.scope_depth() != 0;
        if scoped
            && !matches!(
                leaf.kind(),
                LeafBlockKind::AtxHeading(_)
                    | LeafBlockKind::FencedCode
                    | LeafBlockKind::IndentedCode
                    | LeafBlockKind::Html
                    | LeafBlockKind::Metadata(_)
            )
        {
            self.pending
                .get_or_insert(MarkdownRootLeafPendingReason::NonRootScope);
            return Ok(work); // Unsupported container syntax remains a hole; similar markers grant no authority.
        }
        let source = leaf.source_range();
        let requested = self.byte_coverage.as_ref().unwrap();
        let start = if scoped
            || matches!(
                leaf.kind(),
                LeafBlockKind::FencedCode
                    | LeafBlockKind::IndentedCode
                    | LeafBlockKind::Html
                    | LeafBlockKind::Metadata(_)
            ) {
            leaf.physical_start()
        } else {
            source.start
        }; // Keep proven first-line physical whitespace, without consumed-end neighbor prefixes or unproven containers.
        let intersection = start.max(requested.start)..source.end.min(requested.end);
        if intersection.is_empty() {
            return Ok(work); // Physical bounds may touch a later prefix that this Leaf does not authorize.
        }
        match leaf.kind() {
            LeafBlockKind::AtxHeading(_) => {
                // Use the Leaf's actual heading kind/body/hash facts; the original Plain kernel
                // still identifies special inline syntax.
                let LeafBlockDetails::AtxHeading {
                    opening_marker,
                    lexical_content,
                    attributes,
                    ..
                } = leaf.details()
                else {
                    return Err(MarkdownTableRequestError::InvalidRequest);
                };
                let mut opening = opening_marker.clone();
                if opening.end < source.end {
                    let (byte, measured) = peek(document, opening.end)?;
                    work.source_bytes += measured.source_bytes;
                    work.source_tree_steps += measured.source_tree_steps;
                    work.structural_transitions += measured.structural_transitions;
                    if byte == b' ' {
                        opening.end += 1; // Match the shared helper: include one ASCII space, leaving tabs/extra whitespace literal.
                    }
                }
                self.raw_markers = [Some(opening), None];
                self.heading_next = Some(lexical_content.start);
                if attributes.is_some() {
                    if PlainSemanticJob::supports_closed_atx(&leaf.block) {
                        self.phase = Phase::Coverage;
                    } else {
                        self.pending
                            .get_or_insert(MarkdownRootLeafPendingReason::NeedsGeneralInline);
                        return Ok(work);
                    }
                } else {
                    self.heading_tail = Some(AtxMarkerTail::new(source));
                    self.phase = Phase::HeadingMarkers;
                } // Attribute syntax requires the original complete finite-heading path, not omission or a wiring error.
            }
            LeafBlockKind::FencedCode => {
                self.boundary = Some(FenceBoundaryProof::new(leaf)?);
                self.phase = Phase::Boundary;
            }
            LeafBlockKind::IndentedCode => self.phase = Phase::Coverage,
            LeafBlockKind::Html | LeafBlockKind::Metadata(_) => {
                // The full parser keeps HTML/metadata canonical and literal: no Markdown inline
                // styles, HTML execution, hidden metadata markers, or anchors. The exact <u>
                // allowlist still uses shared Underline, so Coverage cannot bypass pairing and
                // publish default styles early.
                self.phase = Phase::Coverage;
            }
            LeafBlockKind::ThematicBreak => {
                self.raw_markers[0] = Some(source);
                self.phase = Phase::Coverage;
            }
        }
        if scoped {
            self.scoped = Some(ScopedLeaf::new(
                &leaf.block,
                intersection.clone(),
                self.selection_bytes
                    .clone()
                    .expect("Leaf Bounds已完成同一完整选择的字节定位"),
                limits,
                &self.quota,
            )?);
            work.metadata_bytes += std::mem::size_of::<ScopedLeaf>();
            self.phase = Phase::Scope;
        } // Consume actual capture/opening/Origin facts; removing NonRootScope alone would prove nothing.
        self.active_byte_coverage = Some(intersection);
        self.record = Some(record);
        Ok(work)
    }

    /// After proving the byte/UTF-16 intersection, move the unique pairing owner to the actual closed
    /// block so query preparation precedes its subsequent lookahead.
    fn begin_ordinary(
        &mut self,
        ordinary: &mut Option<PlainSemanticJob>,
        budget: usize,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let bytes = PlainSemanticJob::closed_block_metadata();
        if bytes > budget {
            return Err(MarkdownTableRequestError::WorkLimit);
        } // The SourceRequest's fixed slot was jointly reserved; initialize only actual active fields now.
        let SourceRecord::Line(line) = self.record.as_ref().unwrap().as_ref() else {
            return Err(MarkdownTableRequestError::InvalidRequest);
        };
        let requested = self.active_coverage.as_ref().unwrap().clone();
        let byte_coverage = self.active_byte_coverage.as_ref().unwrap().clone();
        if let Some(job) = ordinary.as_mut() {
            if job.retains_plain_block(line.semantic_block.as_ref().unwrap())
                && job.continue_closed_block(
                    requested.clone(),
                    self.selection,
                    byte_coverage.clone(),
                    self.selection_bytes
                        .clone()
                        .expect("Leaf Bounds已完成同一完整选择的字节定位"),
                    &mut self.underline,
                    self.quota.clone(),
                )
            {
                // Retarget this fixed scratch's endpoints/output/quota using the original metadata
                // bound. Actual prefix reads remain later cursor.extend_to/advance work; no new
                // body is prefetched here.
                self.ordinary_run_index = 0;
                self.phase = Phase::Ordinary;
                return Ok(SemanticWork {
                    structural_transitions: CONTROL,
                    metadata_bytes: bytes,
                    ..Default::default()
                });
            }
            // Even the same seed needs a cold path for overlapping/backward ranges. Return its old
            // completed prefix first, then construct the original job next quantum.
            *ordinary = None;
            return Ok(SemanticWork {
                structural_transitions: CONTROL + PlainSemanticJob::completed_drop_control(),
                ..Default::default()
            });
        }
        let underline = self.underline.take().unwrap();
        let job = if line.semantic_block.as_ref().unwrap().is_scoped_paragraph() {
            PlainSemanticJob::for_closed_origin(
                requested,
                self.selection,
                byte_coverage,
                self.selection_bytes
                    .clone()
                    .expect("Leaf Bounds已完成同一完整选择的字节定位"),
                line,
                underline,
                self.quota.clone(),
            )?
        } else {
            PlainSemanticJob::for_closed_block(
                requested,
                self.selection,
                byte_coverage,
                self.selection_bytes
                    .clone()
                    .expect("Leaf Bounds已完成同一完整选择的字节定位"),
                line.semantic_block.as_ref().unwrap(),
                underline,
                self.quota.clone(),
            )?
        };
        *ordinary = Some(job); // Fill the same inline parent slot, without a Box or duplicate empty 4-KiB cursor page.
        self.ordinary_run_index = 0;
        self.phase = Phase::Ordinary;
        Ok(SemanticWork {
            structural_transitions: CONTROL,
            metadata_bytes: bytes,
            ..SemanticWork::default()
        })
    }

    /// Borrow the parent semantic slot for an actual ATX at the measured intersection; this original
    /// Leaf owner remains responsible for scope.
    fn begin_atx_inline(
        &mut self,
        ordinary: &mut Option<PlainSemanticJob>,
        budget: usize,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let bytes = PlainSemanticJob::closed_block_metadata();
        if bytes > budget {
            return Err(MarkdownTableRequestError::WorkLimit);
        }
        let SourceRecord::Leaf(leaf) = self.record.as_ref().unwrap().as_ref() else {
            return Err(MarkdownTableRequestError::InvalidRequest);
        };
        *ordinary = Some(PlainSemanticJob::for_closed_atx(
            self.active_coverage.as_ref().unwrap().clone(),
            self.selection,
            self.active_byte_coverage.as_ref().unwrap().clone(),
            self.selection_bytes
                .clone()
                .expect("Leaf Bounds已完成同一完整选择的字节定位"),
            &leaf.block,
            self.underline.take(),
            self.quota.clone(),
        )?);
        self.ordinary_run_index = 0;
        self.phase = Phase::Ordinary;
        Ok(SemanticWork {
            structural_transitions: CONTROL,
            metadata_bytes: bytes,
            ..Default::default()
        })
    }

    /// Return only this Leaf's temporary coordinates. Successful segments and the request's unique
    /// underline workspace retain their original quota and source identity.
    fn clear_active_leaf(&mut self) {
        self.record = None;
        self.active_byte_coverage = None;
        self.active_coverage = None;
        self.boundary = None;
        self.scoped = None;
        self.ordinary_run = None;
        self.heading_next = None;
        self.heading_tail = None;
        self.projection = None;
        self.raw_markers = [None, None];
        self.markers = None;
    }

    /// Scan the actual ATX body with the shared Plain rules frame by frame. Only a complete body
    /// without candidates can authorize this window's plain-heading intersection.
    fn advance_heading(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let mut work = SemanticWork {
            structural_transitions: CONTROL,
            ..SemanticWork::default()
        };
        let SourceRecord::Leaf(leaf) = self.record.as_ref().unwrap().as_ref() else {
            unreachable!("活动ATX只保原Leaf");
        };
        let LeafBlockDetails::AtxHeading { trim_target, .. } = leaf.details() else {
            return Err(MarkdownTableRequestError::InvalidRequest);
        };
        let next = self.heading_next.unwrap();
        if next == *trim_target {
            self.heading_next = None;
            self.phase = Phase::Coverage;
            return Ok(work); // Empty headings need no neighboring read; nonempty bodies must really reach their original end.
        }
        let tree = tree_height(document);
        let child = budget - CONTROL - tree;
        let frame = document
            .table_source_frame(next, child.min(trim_target - next))
            .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
        let result = leaf.block.plain_heading_content_step(
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
        );
        // Lease-return failure precedes candidate classification; never replace a real reclaim
        // error with NeedsGeneralInline to continue using old functionality.
        frame
            .finish()
            .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
        let (general, measured) = result.map_err(MarkdownTableRequestError::Global)?;
        work.source_bytes = measured.byte_reads;
        work.source_tree_steps = tree;
        work.structural_transitions += measured.transitions + tree;
        if general {
            if PlainSemanticJob::supports_closed_atx(&leaf.block) {
                self.phase = Phase::Coverage; // Finite blocks still require the full parser; heading_next retains the actual candidate phase.
            } else {
                self.pending
                    .get_or_insert(MarkdownRootLeafPendingReason::NeedsGeneralInline);
                self.clear_active_leaf();
                self.phase = Phase::NextLeaf; // Unsupported giant headings publish no run; independent leaves can still complete.
            }
        } else {
            self.heading_next = Some(next + measured.byte_reads);
        }
        Ok(work)
    }
    fn build_one(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let mut work = SemanticWork {
            structural_transitions: CONTROL,
            ..SemanticWork::default()
        };
        let coverage = self.active_coverage.as_ref().unwrap();
        if self.run_next == coverage.end {
            self.phase = Phase::FinishLeaf;
            return Ok(work);
        }
        let SourceRecord::Leaf(leaf) = self.record.as_ref().unwrap().as_ref() else {
            unreachable!("原Leaf不可变");
        };
        let pairs = if matches!(
            leaf.kind(),
            LeafBlockKind::FencedCode | LeafBlockKind::IndentedCode
        ) {
            &[][..] // Proven literal code cannot borrow external HTML pairs to hide <u> bytes or draw underline.
        } else {
            self.underline
                .as_ref()
                .unwrap()
                .ready_pairs_for(document)
                .map_err(MarkdownTableRequestError::Window)?
                .ok_or(MarkdownTableRequestError::InvalidRequest)?
        };
        if pairs
            .get(self.underline_index)
            .is_some_and(|pair| pair.close.end <= self.run_next)
        {
            self.underline_index += 1;
            return Ok(work); // Skip one actual past pair per step, without an unbudgeted loop.
        }
        let mut styles = match leaf.kind() {
            // Map the Leaf's actual level to the six existing style bits, without reparsing heading
            // text or counting hashes again.
            LeafBlockKind::AtxHeading(level) => match level {
                pulldown_cmark::HeadingLevel::H1 => MarkdownPresentationStyles::HEADING_1,
                pulldown_cmark::HeadingLevel::H2 => MarkdownPresentationStyles::HEADING_2,
                pulldown_cmark::HeadingLevel::H3 => MarkdownPresentationStyles::HEADING_3,
                pulldown_cmark::HeadingLevel::H4 => MarkdownPresentationStyles::HEADING_4,
                pulldown_cmark::HeadingLevel::H5 => MarkdownPresentationStyles::HEADING_5,
                pulldown_cmark::HeadingLevel::H6 => MarkdownPresentationStyles::HEADING_6,
            },
            LeafBlockKind::ThematicBreak => MarkdownPresentationStyles::HORIZONTAL_RULE,
            LeafBlockKind::FencedCode | LeafBlockKind::IndentedCode => {
                MarkdownPresentationStyles::FENCED_CODE
            }
            // Markdown spelling remains literal inside actual HTML/metadata. These are the proven
            // kind's presentation semantics, not default styles for unknown or failed candidates.
            LeafBlockKind::Html | LeafBlockKind::Metadata(_) => {
                MarkdownPresentationStyles::default()
            }
        };
        if (self.scoped.is_some()
            || matches!(
                leaf.kind(),
                LeafBlockKind::FencedCode | LeafBlockKind::IndentedCode
            ))
            && self.run_next < self.markers.as_ref().unwrap()[0].start
        {
            styles = MarkdownPresentationStyles::default();
        } // Parser code starts after first-line indentation; preserve canonical whitespace without hiding it or adding code style.
        let mut end = coverage.end;
        if let Some(scoped) = self.scoped.as_ref() {
            let query = scoped.query(self.run_next, end);
            end = query.utf16_range.end;
            styles = styles.union(query.styles);
        }
        for (index, marker) in self.markers.as_ref().unwrap().iter().enumerate() {
            for boundary in [marker.start, marker.end] {
                if boundary > self.run_next {
                    end = end.min(boundary);
                }
            }
            if marker.contains(&self.run_next) {
                styles = styles.union(MarkdownPresentationStyles::MARKER);
                let active = if leaf.kind() == LeafBlockKind::FencedCode {
                    let boundary = self.boundary.as_ref().unwrap();
                    // Use the complete physical-line owner for activity, not clipped marker bounds.
                    // Closing stops before EOL; this same boundary proof read the opening EOL fact.
                    let raw = self.raw_markers[index].as_ref().unwrap();
                    let owner = if index == 0 {
                        leaf.physical_start()..raw.end
                    } else {
                        raw.clone()
                    }; // Opening indentation shares the endpoint line but remains canonical whitespace, not a hidden marker.
                    markdown_physical_line_is_active(
                        self.selection_bytes
                            .as_ref()
                            .expect("Leaf Bounds已完成同一完整选择的字节定位"),
                        &owner,
                        index != 0 || boundary.opening_includes_end,
                    )
                } else {
                    markdown_selection_intersects(
                        self.selection_bytes
                            .as_ref()
                            .expect("Leaf Bounds已完成同一完整选择的字节定位"),
                        &leaf.source_range(),
                    )
                };
                if !active {
                    styles = styles.union(MarkdownPresentationStyles::HIDDEN_MARKER);
                }
            }
        }
        if let Some(pair) = pairs.get(self.underline_index) {
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
            if pair.content.contains(&self.run_next) {
                styles = styles.union(MarkdownPresentationStyles::UNDERLINE);
            }
            if pair.open.contains(&self.run_next) || pair.close.contains(&self.run_next) {
                styles = styles.union(MarkdownPresentationStyles::MARKER);
                if !markdown_selection_intersects(
                    &ordered_selection(self.selection),
                    &(pair.open.start..pair.close.end),
                ) {
                    styles = styles.union(MarkdownPresentationStyles::HIDDEN_MARKER);
                }
            }
        }
        if end <= self.run_next {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        let measured = self.runs.append(
            MarkdownPresentationRun {
                utf16_range: self.run_next..end,
                styles,
            },
            &self.quota,
            budget - CONTROL,
        )?;
        work.structural_transitions += measured.structural_transitions;
        work.metadata_bytes = measured.metadata_bytes;
        self.run_next = end;
        Ok(work)
    }

    /// Report finite publication metadata so the parent reserves fixed cleanup work and actual
    /// payload before ownership transfer.
    pub(super) fn publication_work(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        if !self.done() {
            // An unfinished phase has no publication endpoint; parent source completion cannot
            // substitute for completing these semantics.
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        if let Some(underline) = &self.underline {
            // Keep original source checks; the parent takes back pair/scanner state for ordered
            // handoff rather than disposing it here.
            let _ = underline
                .ready_pairs_for(document)
                .map_err(MarkdownTableRequestError::Window)?;
        }
        Ok(SemanticWork {
            // Small windows allocate no empty pages. Preserve the original conservative control
            // reserve; append separately admits/accounts actual heap directories and pages.
            structural_transitions: CONTROL
                + 2 * PAGE_COUNT
                + 2 * LEAF_PAGE_COUNT
                + self.candidates.len,
            metadata_bytes: if self.coverage.len != 0 {
                std::mem::size_of::<VerifiedMarkdownRootLeafViewport>()
            } else {
                0
            },
            ..SemanticWork::default()
        })
    }

    /// After the parent obtains its actual SourcePublication, check same-source/full-selection
    /// authority and move this finite payload exactly once.
    pub(super) fn publish(
        &mut self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
        publication: Weak<SourcePublication>,
        budget: usize,
    ) -> Result<(RootLeafSemanticPublication, SemanticWork), MarkdownTableRequestError> {
        if let Err(error) = self.check(document, authority) {
            self.fail(error); // Actual source/cancellation/full-selection revocation stays primary; old parameters cannot revive publication.
            return Err(error);
        }
        let original = publication
            .upgrade()
            .ok_or(MarkdownTableRequestError::PublicationSuperseded)?;
        if !original.source.is_current(document)
            || original.retired.get()
            || original.context_revision != self.context_revision
            || original.selection != self.selection
            || !self.done()
        {
            // The original publication and full selection jointly authorize transfer; another
            // context with equal coordinates cannot bless these results.
            return Err(MarkdownTableRequestError::PublicationSuperseded);
        }
        let work = self.publication_work(document)?;
        if work.structural_transitions > budget || work.metadata_bytes > budget {
            return Err(MarkdownTableRequestError::WorkLimit);
        }
        // Ready retains no context/pair Vec. The parent takes back the same scanner after this
        // publication, without a second ownership graph.
        self.boundary = None;
        self.scoped = None;
        self.ordinary_run = None;
        self.projection = None;
        self.candidates = LeafPages::new(); // This publication step accounts candidate Rc/page cleanup; retain no duplicate source inventory.
        let result = if self.coverage.len == 0 {
            RootLeafSemanticPublication::Pending(
                self.pending
                    .ok_or(MarkdownTableRequestError::InvalidRequest)?,
            ) // Empty results keep their explicit pending reason rather than an empty Ready directory.
        } else {
            if self.runs.len == 0 || self.record.is_some() || self.active_coverage.is_some() {
                return Err(MarkdownTableRequestError::InvalidRequest);
            } // Move partial coverage only after every proven segment completes and no active Leaf remains.
            RootLeafSemanticPublication::Ready(Box::new(VerifiedMarkdownRootLeafViewport {
                source: self.source.clone(),
                context_revision: self.context_revision,
                selection: self.selection,
                publication,
                requested: self.requested.clone(),
                coverage: std::mem::replace(&mut self.coverage, LeafPages::new()),
                runs: std::mem::replace(&mut self.runs, RunPages::new()),
                links: std::mem::take(&mut self.links),
            }))
        };
        self.phase = Phase::Published;
        Ok((result, work))
    }
}

/// Bounded reverse reads for the original Leaf, sharing HeadingMarkerTail with existing parser
/// presentation decisions.
struct AtxMarkerTail {
    source: Range<usize>,
    tail: Option<HeadingMarkerTail>,
}
impl AtxMarkerTail {
    fn new(source: Range<usize>) -> Self {
        Self { source, tail: None }
    }

    fn done(&self) -> bool {
        self.tail.as_ref().is_some_and(HeadingMarkerTail::done)
    }

    fn marker(&self) -> Option<Range<usize>> {
        self.tail.as_ref().and_then(HeadingMarkerTail::marker)
    }

    /// Borrow one actual tail fragment per call and report Source/tree work to the original parent,
    /// without copying giant whitespace/hash suffixes.
    fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let mut work = SemanticWork {
            structural_transitions: CONTROL,
            ..SemanticWork::default()
        };
        let Some(tail) = self.tail.as_mut() else {
            let (byte, measured) = peek(document, self.source.end - 1)?;
            let end = self.source.end - usize::from(byte == b'\n');
            // The existing first_line helper strips only the first raw LF; the original ATX Leaf
            // already proves a single-line boundary, leaving CR to the shared tail rules.
            self.tail = Some(HeadingMarkerTail::new(self.source.start, end));
            return Ok(measured);
        };
        if tail.done() {
            return Ok(work); // Completion transfers fixed ranges without borrowing again or restarting tail scanning.
        }
        let tree = tree_height(document);
        let wanted = child_budget(budget - CONTROL, tree)? / 4;
        let (_start, bytes) = backward_frame(document, tail.next_end(), self.source.start, wanted)?;
        work.source_tree_steps = tree;
        work.structural_transitions += tree;
        for &byte in bytes.iter().rev() {
            tail.observe_reverse(byte);
            work.source_bytes += 1;
            work.structural_transitions += 2;
            if tail.done() {
                break; // Only shared rules finish presentation; frame exhaustion preserves the actual next position.
            }
        }
        bytes
            .finish()
            .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
        Ok(work)
    }
}

/// The original producer proved opening/body/actual closer. Read only the opening's actual EOL
/// ownership; do not rescan the final body or reinterpret literal EOF fences as closers.
struct FenceBoundaryProof {
    opening: Range<usize>,
    opening_includes_end: bool,
    closing: Option<Range<usize>>,
    done: bool,
}

impl FenceBoundaryProof {
    fn new(leaf: &VerifiedMarkdownOrdinaryLeaf) -> Result<Self, MarkdownTableRequestError> {
        let LeafBlockDetails::FencedCode {
            opening_line,
            body_enclosure,
            closing_marker,
            closing_scanner_end,
            ..
        } = leaf.details()
        else {
            return Err(MarkdownTableRequestError::InvalidRequest);
        };
        let source = leaf.source_range();
        if opening_line.start != source.start
            || opening_line.start >= opening_line.end
            || opening_line.end > source.end
            || body_enclosure.start != opening_line.end
        {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        let closing = match (closing_marker, closing_scanner_end) {
            (Some(marker), Some(end))
                if body_enclosure.end <= marker.start
                    && marker.start < marker.end
                    && marker.end <= *end
                    && *end == source.end
                    && leaf.close_reason() == LeafBlockCloseReason::ExplicitMarker =>
            {
                Some(body_enclosure.end..*end)
            }
            (None, None)
                if matches!(
                    leaf.close_reason(),
                    LeafBlockCloseReason::TrueEof | LeafBlockCloseReason::ContainerExit
                ) =>
            {
                None
            }
            _ => return Err(MarkdownTableRequestError::InvalidRequest),
        };
        Ok(Self {
            opening: opening_line.clone(),
            opening_includes_end: false,
            closing,
            done: false,
        })
    }

    fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let (byte, work) = peek(document, self.opening.end - 1)?;
        self.opening_includes_end = !matches!(byte, b'\r' | b'\n');
        self.done = true;
        Ok(work)
    }
}

fn peek(
    document: &MarkdownDocumentBuffer,
    at: usize,
) -> Result<(u8, SemanticWork), MarkdownTableRequestError> {
    let tree = tree_height(document);
    let frame = document
        .table_source_frame(at, 1)
        .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
    let byte = *frame
        .bytes
        .first()
        .ok_or(MarkdownTableRequestError::InvalidRequest)?;
    frame
        .finish()
        .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
    Ok((
        byte,
        SemanticWork {
            source_bytes: 1,
            source_tree_steps: tree,
            structural_transitions: CONTROL + tree + 2,
            ..SemanticWork::default()
        },
    ))
}

/// Borrow one actual reverse tail fragment from the same piece root, without copying, reading before
/// source.start, or treating an arbitrary frame boundary as EOF.
fn backward_frame(
    document: &MarkdownDocumentBuffer,
    end: usize,
    floor: usize,
    maximum: usize,
) -> Result<(usize, PieceBytes<'_>), MarkdownTableRequestError> {
    if floor >= end || end > document.byte_len() || maximum == 0 || maximum > 64 * 1_024 {
        return Err(MarkdownTableRequestError::InvalidRequest);
    }
    let mut local = end - 1;
    let mut node = document
        .root
        .as_deref()
        .ok_or(MarkdownTableRequestError::InvalidRequest)?;
    loop {
        match node {
            MarkdownPieceTreeNode::Branch { left, right, .. } => {
                let length = left.metrics().byte_len;
                if local < length {
                    node = left;
                } else {
                    local -= length;
                    node = right;
                }
            }
            MarkdownPieceTreeNode::OriginalSpan(span) => {
                // Reverse reads borrow only the hit fragment, never a preceding shared directory
                // mistaken for a contiguous 64-KiB body.
                let hit = span
                    .locate_byte(local, false)
                    .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
                let local_end = hit.local + 1;
                let count = maximum.min(local_end).min(end - floor);
                let bytes = document
                    .piece_range_bytes(&hit.piece, local_end - count..local_end)
                    .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
                return Ok((end - count, bytes));
            }
            MarkdownPieceTreeNode::Leaf(piece) => {
                let local_end = local + 1;
                let count = maximum.min(local_end).min(end - floor);
                let bytes = document
                    .piece_range_bytes(piece, local_end - count..local_end)
                    .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
                return Ok((end - count, bytes));
            }
        }
    }
}

#[cfg(test)]
mod tests;
