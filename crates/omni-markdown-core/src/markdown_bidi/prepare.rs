//! A suspendable pure Core driver for the original UBA; compiler-owned loop locals survive without unwinding and replaying already modified rules.
use super::*;
use std::{
    future::Future,
    pin::Pin,
    rc::Rc,
    task::{Context, Poll, Waker},
};

pub(super) mod cooperate;
pub(super) mod io;
pub(super) mod source;
use cooperate::Gate;
use io::CooperativeStore;

#[cfg(all(test, feature = "paged-layout-breaks"))]
mod tests;

/// Independent work limits for one explicit step; source bytes/scalars are charged separately and a UTF8 window edge is not a paragraph end.
#[derive(Clone, Copy, Debug)]
pub struct MarkdownBidiPrepareBudget {
    pub bytes: usize,
    pub scalars: usize,
    /// One unit per admitted 32-byte record read/write or resource allocation/flush attempt; this does not count individual OS syscalls.
    pub metadata_ops: usize,
}
impl MarkdownBidiPrepareBudget {
    fn empty() -> Self {
        Self {
            bytes: 0,
            scalars: 0,
            metadata_ops: 0,
        }
    }
}
impl Default for MarkdownBidiPrepareBudget {
    fn default() -> Self {
        Self {
            bytes: 4096,
            scalars: 1024,
            metadata_ops: 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownBidiPrepareStatus {
    Pending,
    Ready,
}

/// Count work admitted/performed by this poll; failure retains work already done instead of reporting a false zero. Counts are not elapsed time or RSS.
#[derive(Clone, Copy, Debug)]
pub struct MarkdownBidiPrepareStep {
    pub status: MarkdownBidiPrepareStatus,
    pub bytes: usize,
    pub scalars: usize,
    pub metadata_ops: usize,
    pub record_reads: usize,
    pub record_writes: usize,
    pub resource_ops: usize,
    pub source_borrow_max_bytes: usize,
}
impl MarkdownBidiPrepareStep {
    fn pending() -> Self {
        Self {
            status: MarkdownBidiPrepareStatus::Pending,
            bytes: 0,
            scalars: 0,
            metadata_ops: 0,
            record_reads: 0,
            record_writes: 0,
            resource_ops: 0,
            source_borrow_max_bytes: 0,
        }
    }
}

type Computation = Pin<Box<dyn Future<Output = Result<MarkdownBidiParagraph, BidiError>>>>;

/// The actual range is bound once at construction, without a public request capable of reauthorizing partial UBA results.
struct Seed {
    snapshot: MarkdownTextSnapshot,
    range: Range<usize>,
    bytes: Range<usize>,
    base: BidiBaseDirection,
    budget: BidiMemoryBudget,
    cancellation: MarkdownBidiCancellation,
    #[cfg(feature = "paged-layout-breaks")]
    layout_source: Option<crate::MarkdownLayoutTextSource>,
    #[cfg(feature = "paged-shaping-frontier")]
    folds: Option<crate::markdown_layout_source::folds::FoldPlan>,
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    tables: Option<crate::markdown_layout_source::table_rows::TablePlan>,
}

/// Copy only a stable cause without text or business closures; the first return still delivers the original full Storage error.
#[derive(Clone, Copy)]
pub(super) enum Failure {
    Cancelled,
    SourceMismatch,
    InvalidRange,
    FoldedRange,
    TableRange,
    UnpreparedRange,
    InvalidScalarBoundary,
    InvalidBudget,
    CoordinateOverflow,
    CorruptMetadata,
    PoisonedMetadata,
    Source(MarkdownDocumentBufferError),
    Storage(Option<i32>, std::io::ErrorKind),
}
impl Failure {
    pub(super) fn capture(error: &BidiError) -> Self {
        // Preserve typed stable variants and Storage errno/kind, rather than retaining an arbitrary error payload across the failed owner's lifetime.
        match error {
            BidiError::Cancelled => Self::Cancelled,
            BidiError::SourceMismatch => Self::SourceMismatch,
            BidiError::InvalidRange => Self::InvalidRange,
            BidiError::FoldedRange => Self::FoldedRange,
            BidiError::TableRange => Self::TableRange,
            BidiError::UnpreparedRange => Self::UnpreparedRange,
            BidiError::InvalidScalarBoundary => Self::InvalidScalarBoundary,
            BidiError::InvalidBudget => Self::InvalidBudget,
            BidiError::CoordinateOverflow => Self::CoordinateOverflow,
            BidiError::CorruptMetadata => Self::CorruptMetadata,
            BidiError::PoisonedMetadata => Self::PoisonedMetadata,
            BidiError::Source(error) => Self::Source(*error),
            BidiError::Storage(error) => Self::Storage(error.raw_os_error(), error.kind()),
        }
    }
    pub(super) fn error(self) -> BidiError {
        // Repeated queries reconstruct the same stable failure category; a Storage error without errno retains its kind, not its original message object.
        match self {
            Self::Cancelled => BidiError::Cancelled,
            Self::SourceMismatch => BidiError::SourceMismatch,
            Self::InvalidRange => BidiError::InvalidRange,
            Self::FoldedRange => BidiError::FoldedRange,
            Self::TableRange => BidiError::TableRange,
            Self::UnpreparedRange => BidiError::UnpreparedRange,
            Self::InvalidScalarBoundary => BidiError::InvalidScalarBoundary,
            Self::InvalidBudget => BidiError::InvalidBudget,
            Self::CoordinateOverflow => BidiError::CoordinateOverflow,
            Self::CorruptMetadata => BidiError::CorruptMetadata,
            Self::PoisonedMetadata => BidiError::PoisonedMetadata,
            Self::Source(error) => BidiError::Source(error),
            Self::Storage(errno, kind) => BidiError::Storage(errno.map_or_else(
                || std::io::Error::from(kind),
                std::io::Error::from_raw_os_error,
            )),
        }
    }
}

/// One fixed future and original Store inside its actual worker; this value promises no Send and creates neither executor nor background thread.
///
/// Move Source/cancellation into the original worker before constructing this job there. Awaits only wait for the next explicit step's quota,
/// without a timer, Native waker, polling service, or another text model. Drop destroys the actual future and returns its Store, Source,
/// and fixed pages; observers retain atomic facts only.
pub struct MarkdownBidiPrepareJob {
    gate: Rc<Gate>,
    computation: Option<Computation>,
    ready: Option<MarkdownBidiParagraph>,
    resources: MarkdownBidiResourceObserver,
    failure: Option<Failure>,
    consumed: bool,
    #[cfg(feature = "paged-layout-breaks")]
    authority: Option<crate::MarkdownLayoutTextAuthority>,
}

impl MarkdownBidiPrepareJob {
    /// Accept only the complete original Source; byte zero/EOF come from the same frozen tree totals without hidden endpoint-leaf reads.
    #[cfg(feature = "paged-layout-breaks")]
    pub fn new_layout_source(
        source: crate::MarkdownLayoutTextSource,
        authority: &crate::MarkdownLayoutTextAuthority,
        base: BidiBaseDirection,
        budget: BidiMemoryBudget,
        cancellation: &MarkdownBidiCancellation,
    ) -> Result<Self, BidiError> {
        let (job, _) =
            Self::new_layout_source_observed(source, authority, base, budget, cancellation);
        job
    }

    /// Obtain the original resource scope before any FD acquisition; a failed constructor still returns its observer.
    #[cfg(feature = "paged-layout-breaks")]
    pub(crate) fn new_layout_source_observed(
        source: crate::MarkdownLayoutTextSource,
        authority: &crate::MarkdownLayoutTextAuthority,
        base: BidiBaseDirection,
        budget: BidiMemoryBudget,
        cancellation: &MarkdownBidiCancellation,
    ) -> (Result<Self, BidiError>, MarkdownBidiResourceObserver) {
        Self::layout_source_observed(
            source,
            authority,
            base,
            budget,
            cancellation,
            #[cfg(feature = "paged-shaping-frontier")]
            None,
            #[cfg(all(
                feature = "s2-table-window-prototype",
                feature = "paged-shaping-frontier"
            ))]
            None,
        )
    }

    /// Fold composition establishes an explicit visible-paragraph scope only; the first actual level still needs later original paragraph quanta.
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn new_layout_source_folded_observed(
        source: crate::MarkdownLayoutTextSource,
        authority: &crate::MarkdownLayoutTextAuthority,
        folds: crate::markdown_layout_source::folds::FoldPlan,
        base: BidiBaseDirection,
        budget: BidiMemoryBudget,
        cancellation: &MarkdownBidiCancellation,
    ) -> (Result<Self, BidiError>, MarkdownBidiResourceObserver) {
        Self::layout_source_observed(
            source,
            authority,
            base,
            budget,
            cancellation,
            Some(folds),
            #[cfg(feature = "s2-table-window-prototype")]
            None,
        )
    }

    /// Original Body flow declares a dynamic structural table directory only; UBA still prepares the actual complete B paragraph in quanta.
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    pub(crate) fn new_layout_source_table_observed(
        source: crate::MarkdownLayoutTextSource,
        authority: &crate::MarkdownLayoutTextAuthority,
        tables: crate::markdown_layout_source::table_rows::TablePlan,
        folds: Option<crate::markdown_layout_source::folds::FoldPlan>,
        base: BidiBaseDirection,
        budget: BidiMemoryBudget,
        cancellation: &MarkdownBidiCancellation,
    ) -> (Result<Self, BidiError>, MarkdownBidiResourceObserver) {
        Self::layout_source_observed(
            source,
            authority,
            base,
            budget,
            cancellation,
            folds,
            Some(tables),
        )
    }

    /// All Source composition uses the same Seed/future construction, differing only in explicit display proofs.
    #[cfg(feature = "paged-layout-breaks")]
    fn layout_source_observed(
        source: crate::MarkdownLayoutTextSource,
        authority: &crate::MarkdownLayoutTextAuthority,
        base: BidiBaseDirection,
        budget: BidiMemoryBudget,
        cancellation: &MarkdownBidiCancellation,
        #[cfg(feature = "paged-shaping-frontier")] folds: Option<
            crate::markdown_layout_source::folds::FoldPlan,
        >,
        #[cfg(all(
            feature = "s2-table-window-prototype",
            feature = "paged-shaping-frontier"
        ))]
        tables: Option<crate::markdown_layout_source::table_rows::TablePlan>,
    ) -> (Result<Self, BidiError>, MarkdownBidiResourceObserver) {
        let resources = resource::ResourceState::new();
        let observer = resources.observe();
        let result = (|| {
            source.health()?;
            if !source.permits(authority) {
                return Err(BidiError::SourceMismatch);
            }
            #[cfg(feature = "paged-shaping-frontier")]
            if folds
                .as_ref()
                .is_some_and(|plan| !source.permits(plan.authority()))
            {
                return Err(BidiError::SourceMismatch);
            }
            cancellation.check()?;
            #[cfg(all(
                feature = "s2-table-window-prototype",
                feature = "paged-shaping-frontier"
            ))]
            if let Some(tables) = &tables {
                tables.require(&source)?;
            }
            let snapshot = source.snapshot_for_bidi();
            let utf16 = 0..source.utf16_len();
            let bytes = 0..source.byte_len();
            let mut job = Self::with_endpoints(
                Seed {
                    snapshot,
                    range: utf16,
                    bytes,
                    base,
                    budget,
                    cancellation: cancellation.clone(),
                    layout_source: Some(source),
                    #[cfg(feature = "paged-shaping-frontier")]
                    folds,
                    #[cfg(all(
                        feature = "s2-table-window-prototype",
                        feature = "paged-shaping-frontier"
                    ))]
                    tables,
                },
                Arc::clone(&resources),
            )?;
            job.authority = Some(authority.clone());
            Ok(job)
        })();
        (result, observer)
    }

    /// The original bounded cursor already certified a complete B paragraph and both coordinate ranges; do not synchronously reread boundary leaves here.
    #[cfg(feature = "paged-shaping-frontier")]
    pub(super) fn new_layout_paragraph_observed(
        source: crate::MarkdownLayoutTextSource,
        authority: &crate::MarkdownLayoutTextAuthority,
        endpoints: (Range<usize>, Range<usize>),
        base: BidiBaseDirection,
        budget: BidiMemoryBudget,
        cancellation: &MarkdownBidiCancellation,
        resources: Arc<resource::ResourceState>,
    ) -> Result<Self, BidiError> {
        source.health()?;
        if !source.permits(authority) {
            return Err(BidiError::SourceMismatch);
        }
        cancellation.check()?;
        let snapshot = source.snapshot_for_bidi();
        let mut job = Self::with_endpoints(
            Seed {
                snapshot,
                range: endpoints.0,
                bytes: endpoints.1,
                base,
                budget,
                cancellation: cancellation.clone(),
                layout_source: Some(source),
                folds: None,
                #[cfg(feature = "s2-table-window-prototype")]
                tables: None,
            },
            resources,
        )?;
        job.authority = Some(authority.clone());
        Ok(job)
    }

    /// The synchronous snapshot/range entry performs its bounded endpoint-leaf lookup here, then drives the same state machine.
    /// This initialization does not claim the fine source-byte admission of the public Prepare entry.
    pub(super) fn snapshot_range(
        snapshot: MarkdownTextSnapshot,
        range: Range<usize>,
        base: BidiBaseDirection,
        budget: BidiMemoryBudget,
        cancellation: &MarkdownBidiCancellation,
        resources: Option<&Arc<resource::ResourceState>>,
    ) -> Result<Self, BidiError> {
        snapshot.original_backing_health()?;
        cancellation.check()?;
        if range.start > range.end || range.end > snapshot.utf16_len() {
            return Err(BidiError::InvalidRange);
        }
        let bytes = snapshot
            .byte_offset_for_utf16(range.start)
            .map_err(scalar_endpoint_error)?
            ..snapshot
                .byte_offset_for_utf16(range.end)
                .map_err(scalar_endpoint_error)?;
        Self::with_endpoints(
            Seed {
                snapshot,
                range,
                bytes,
                base,
                budget,
                cancellation: cancellation.clone(),
                #[cfg(feature = "paged-layout-breaks")]
                layout_source: None,
                #[cfg(feature = "paged-shaping-frontier")]
                folds: None,
                #[cfg(all(
                    feature = "s2-table-window-prototype",
                    feature = "paged-shaping-frontier"
                ))]
                tables: None,
            },
            resources
                .cloned()
                .unwrap_or_else(resource::ResourceState::new),
        )
    }

    fn with_endpoints(
        seed: Seed,
        resources: Arc<resource::ResourceState>,
    ) -> Result<Self, BidiError> {
        seed.snapshot.original_backing_health()?;
        seed.cancellation.check()?;
        // Internal records use u32 IDs with NONE reserved; reject unrepresentable input rather than colliding with the sentinel.
        let units = u32::try_from(seed.range.len()).map_err(|_| BidiError::CoordinateOverflow)?;
        if units == NONE {
            return Err(BidiError::CoordinateOverflow);
        }
        let gate = Gate::new(&seed.snapshot, seed.cancellation.clone());
        let computation = Box::pin(compute(
            seed,
            Arc::clone(&resources),
            Rc::clone(&gate),
            units,
        ));
        Ok(Self {
            gate,
            computation: Some(computation),
            ready: None,
            resources: resources.observe(),
            failure: None,
            consumed: false,
            #[cfg(feature = "paged-layout-breaks")]
            authority: None,
        })
    }

    #[cfg(feature = "paged-layout-breaks")]
    pub fn step(
        &mut self,
        authority: &crate::MarkdownLayoutTextAuthority,
        budget: MarkdownBidiPrepareBudget,
    ) -> Result<MarkdownBidiPrepareStep, BidiError> {
        if let Err(error) = self.gate.source_health() {
            return self.fail(error);
        }
        if self.authority.as_ref() != Some(authority) {
            return self.fail(BidiError::SourceMismatch);
        }
        self.advance(budget)
    }

    fn advance(
        &mut self,
        budget: MarkdownBidiPrepareBudget,
    ) -> Result<MarkdownBidiPrepareStep, BidiError> {
        if let Err(error) = self.gate.source_health() {
            return self.fail(error);
        }
        if let Some(failure) = self.failure {
            return Err(failure.error()); // Later cancellation or an invalid quota cannot erase an already recorded metadata failure.
        }
        if let Err(error) = self.gate.health() {
            return self.fail(error);
        }
        if self.consumed {
            return Err(BidiError::PoisonedMetadata); // A consumed result cannot be issued again by supplying another budget.
        }
        if budget.bytes == 0 || budget.scalars == 0 || budget.metadata_ops == 0 {
            return self.fail(BidiError::InvalidBudget);
        }
        self.gate.replenish(budget);
        if let Some(ready) = &self.ready {
            let healthy = ready
                ._snapshot
                .original_backing_health()
                .map_err(BidiError::from)
                .and_then(|()| ready.resolution.healthy());
            if let Err(error) = healthy {
                return self.fail(error);
            }
            let mut step = self.gate.progress();
            step.status = MarkdownBidiPrepareStatus::Ready;
            return Ok(step); // A cached Ready does not rerun the future, but original health and cancellation remain required.
        }
        let mut context = Context::from_waker(Waker::noop());
        let result = self
            .computation
            .as_mut()
            .ok_or(BidiError::PoisonedMetadata)?
            .as_mut()
            .poll(&mut context);
        // Actual original-source failure has priority after polling too; subsequent cancellation cannot erase a returned metadata cause.
        if let Err(error) = self.gate.source_health() {
            return self.fail(error);
        }
        match result {
            Poll::Ready(Err(error)) => self.fail(error),
            Poll::Ready(Ok(ready)) => {
                // Recheck the same gate after all actual computation; failed final authorization drops the actual candidate instead of publishing it.
                if let Err(error) = self.gate.health() {
                    drop(ready);
                    return self.fail(error);
                }
                self.computation.take();
                self.ready = Some(ready);
                let mut progress = self.gate.progress();
                progress.status = MarkdownBidiPrepareStatus::Ready;
                Ok(progress)
            }
            Poll::Pending => {
                // Pending keeps the original future's actual local state; a final health failure still destroys that owner immediately.
                if let Err(error) = self.gate.health() {
                    return self.fail(error);
                }
                Ok(self.gate.progress())
            }
        }
    }

    fn fail<T>(&mut self, error: BidiError) -> Result<T, BidiError> {
        self.computation.take(); // Drop the original future and its actual local Store/borrows/Source rather than clearing immutable text elsewhere.
        self.ready.take();
        if let Err(source) = self.gate.source_health() {
            self.failure = Some(Failure::capture(&source));
            return Err(source); // Even backing failure discovered during final Drop precedes the earlier cancellation or metadata outcome.
        }
        if let Some(failure) = self.failure {
            return Err(failure.error());
        }
        // The existing contract retains an original Storage error; otherwise an observed failed close becomes Storage. It does not retain both errors.
        let error = if !matches!(error, BidiError::Storage(_))
            && let Some(errno) = self.resources.snapshot().close_errno
        {
            BidiError::Storage(std::io::Error::from_raw_os_error(errno))
        } else {
            error
        };
        self.failure = Some(Failure::capture(&error));
        Err(error)
    }

    #[cfg(feature = "paged-layout-breaks")]
    pub fn take_ready(
        &mut self,
        authority: &crate::MarkdownLayoutTextAuthority,
    ) -> Result<Option<MarkdownBidiParagraph>, BidiError> {
        if let Err(error) = self.gate.source_health() {
            return self.fail(error);
        }
        if self.authority.as_ref() != Some(authority) {
            return self.fail(BidiError::SourceMismatch);
        }
        if let Some(failure) = self.failure {
            return Err(failure.error());
        }
        if let Err(error) = self.gate.health() {
            return self.fail(error);
        }
        if self.consumed {
            return Err(BidiError::PoisonedMetadata);
        }
        let ready = self.ready.take();
        self.consumed = ready.is_some();
        Ok(ready) // Pending returns None only; no partial rule or future SCX/UBA prefix gains Ready authority.
    }
    pub fn resource_observer(&self) -> MarkdownBidiResourceObserver {
        self.resources.clone()
    }

    /// The visible-paragraph owner checks the original job's permanent state without replenishing quota, polling the future, or transferring Ready.
    #[cfg(feature = "paged-shaping-frontier")]
    pub(super) fn require_healthy(&self) -> Result<(), BidiError> {
        self.gate.source_health()?;
        if let Some(failure) = self.failure {
            return Err(failure.error());
        }
        self.gate.health()
    }
    /// Inspect last actual work after failure too; these fixed diagnostics grant neither Source nor partial-level authority.
    pub fn last_step(&self) -> MarkdownBidiPrepareStep {
        self.gate.progress()
    }
    pub(super) fn run(mut self) -> Result<MarkdownBidiParagraph, BidiError> {
        // The synchronous API drives the same admitted computation to completion; its loop is not a promise of interactive yield or bounded total latency.
        loop {
            let step = self.advance(MarkdownBidiPrepareBudget::default())?;
            if step.status == MarkdownBidiPrepareStatus::Ready {
                self.gate.health()?;
                return self.ready.take().ok_or(BidiError::PoisonedMetadata);
            }
        }
    }
}

async fn compute(
    seed: Seed,
    resources: Arc<resource::ResourceState>,
    gate: Rc<Gate>,
    units: u32,
) -> Result<MarkdownBidiParagraph, BidiError> {
    let Seed {
        snapshot,
        range: source,
        bytes,
        base,
        budget,
        cancellation,
        #[cfg(feature = "paged-layout-breaks")]
        layout_source,
        #[cfg(feature = "paged-shaping-frontier")]
        folds,
        #[cfg(all(
            feature = "s2-table-window-prototype",
            feature = "paged-shaping-frontier"
        ))]
        tables,
    } = seed;
    snapshot.original_backing_health()?;
    gate.health()?;
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    if let Some(tables) = tables {
        // The table directory may initially be empty; later rows only revoke their own interior authority, without a document prescan or replacement Source.
        return Ok(MarkdownBidiParagraph {
            _snapshot: snapshot,
            layout_source,
            source,
            resolution: Resolution::VisibleParagraphs(Box::new(
                visible::VisibleParagraphs::new_tables(
                    tables,
                    folds,
                    base,
                    budget,
                    cancellation,
                    resources,
                ),
            )),
            units,
            paragraph_count: None,
            source_borrow_max: 0,
            word_order: visible::WordOrder::default(),
        });
    }
    #[cfg(feature = "paged-shaping-frontier")]
    let certificate = if folds.as_ref().is_some_and(|plan| !plan.spans().is_empty()) {
        uniform::root::certify(&snapshot, &source, &bytes, base)?
    } else {
        uniform::certify_prepared(&snapshot, &source, bytes.clone(), base, &gate).await?
    };
    #[cfg(not(feature = "paged-shaping-frontier"))]
    let certificate =
        uniform::certify_prepared(&snapshot, &source, bytes.clone(), base, &gate).await?;
    if let Some(certificate) = certificate {
        snapshot.original_backing_health()?;
        gate.health()?;
        let paragraph_count = if certificate.has_paragraph_boundaries() {
            None // Locate actual B ranges on demand without inventing a precomputed total or creating another paragraph directory.
        } else {
            Some(u32::from(units != 0))
        };
        return Ok(MarkdownBidiParagraph {
            _snapshot: snapshot,
            #[cfg(feature = "paged-layout-breaks")]
            layout_source,
            source,
            resolution: Resolution::UniformZero(certificate.seal(cancellation)),
            units,
            paragraph_count,
            source_borrow_max: gate.source_borrow_max(),
            #[cfg(feature = "paged-shaping-frontier")]
            word_order: visible::WordOrder::default(),
        });
    }
    #[cfg(feature = "paged-shaping-frontier")]
    if let Some(plan) = folds.filter(|plan| !plan.spans().is_empty()) {
        // If the complete root cannot prove level zero, establish only a visible-paragraph scope; do not scan hidden RTL/controls to obtain a fast path.
        snapshot.original_backing_health()?;
        gate.health()?;
        return Ok(MarkdownBidiParagraph {
            _snapshot: snapshot,
            layout_source,
            source,
            resolution: Resolution::VisibleParagraphs(Box::new(visible::VisibleParagraphs::new(
                plan,
                base,
                budget,
                cancellation,
                resources,
            ))),
            units,
            paragraph_count: None,
            source_borrow_max: 0,
            word_order: visible::WordOrder::default(),
        });
    }
    let mut store =
        CooperativeStore::new(units, budget, cancellation, &resources, Rc::clone(&gate)).await?;
    // Capture complete requested coordinates into the sole metadata Store, then run the ordered UBA phases over that original paged inventory.
    let paragraph_count =
        super::source::capture_prepared(&snapshot, &source, bytes, base, &mut store).await?;
    for id in 0..paragraph_count {
        let paragraph = Paragraph::read_prepared(&mut store, id).await?;
        let (first, end) = explicit::compute(&mut store, paragraph).await?;
        let count = sequences::compute(&mut store, paragraph, first..end).await?;
        for sequence in 0..count {
            let sequence = Sequence::read_prepared(&mut store, sequence).await?;
            implicit::resolve(&mut store, sequence).await?;
        }
        implicit::finish_levels(&mut store, paragraph).await?;
    }
    store.flush().await?;
    // All dirty metadata must reach the original file and pass actual Source/Store checks before its completed owner is published.
    snapshot.original_backing_health()?;
    store.healthy()?;
    Ok(MarkdownBidiParagraph {
        _snapshot: snapshot,
        #[cfg(feature = "paged-layout-breaks")]
        layout_source,
        source,
        resolution: Resolution::Paged(store.into_inner()),
        units,
        paragraph_count: Some(paragraph_count),
        source_borrow_max: gate.source_borrow_max(),
        #[cfg(feature = "paged-shaping-frontier")]
        word_order: visible::WordOrder::default(),
    })
}
