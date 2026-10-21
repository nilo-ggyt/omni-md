//! Coordinate actual itemization, the unique Store, and original Measure in one
//! cancellable worker. Public intermediate states never imply completed layout.
use super::store::resolved::ResolvedCapture;
use super::{
    item::Result,
    store::{GlyphStore, StoredItemProof},
    *,
};
use crate::markdown_soft_line_measure::provider::SelectedLineStart;
use crate::markdown_soft_line_resolve::stored::MarkdownStoredPrefixResolveJob;
use crate::{
    MarkdownEmergencyGlyphSliceAnswer, MarkdownLayoutTextAuthority, MarkdownLayoutTextSource,
    MarkdownOriginalBackingObserver, MarkdownSoftLineMeasureAnswer, MarkdownSoftLineMeasureBudget,
    MarkdownSoftLineMeasureChoice, MarkdownSoftLineMeasureJob, MarkdownSoftLineMeasureRequest,
    MarkdownSoftLineResolveBudget,
};

#[cfg(feature = "paged-shaping-frontier")]
pub(super) mod word;
#[cfg(feature = "paged-shaping-frontier")]
pub use word::{
    MarkdownWordDeleteAnswer, MarkdownWordDeleteDirection, MarkdownWordDeleteStep,
    MarkdownWordGroupAction, MarkdownWordGroupAnswer, MarkdownWordGroupStep,
    MarkdownWordMovePreparation, MarkdownWordMoveScope,
};

#[cfg(all(
    feature = "s2-table-window-prototype",
    feature = "paged-shaping-frontier"
))]
pub(super) mod table_row;
#[cfg(all(
    feature = "s2-table-window-prototype",
    feature = "paged-shaping-frontier"
))]
pub use table_row::{MarkdownLineTableRowRequest, MarkdownLineTableRowResponse};

/// Only an actual committed LineEnd produces LineReady. Pending controls, unsafe
/// boundaries, dictionary work, and missing glyphs retain their original stages.
#[derive(Clone)]
pub enum MarkdownLineComposeAnswer {
    Yielded,
    NeedStyle(MarkdownLineStyleRequest),
    LineReady(MarkdownComposedLineFacts),
    Complete,
    NeedsControl(MarkdownLineInlineControl),
    NeedsResolution(MarkdownSoftLineMeasureChoice),
    NeedsTabPlacement(MarkdownLineTabPlacement),
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    NeedsTableRow(MarkdownLineTableRowRequest),
    #[cfg(feature = "paged-shaping-frontier")]
    NeedsFrontier(MarkdownLineFrontierReason),
}
pub struct MarkdownLineComposeStep {
    pub bytes: usize,
    /// This step only consumed published directories/glyphs, allowing the scheduler
    /// to continue under its remaining quota without granting more Source reads.
    /// Unproven stages stay false: zero reported bytes alone cannot establish that
    /// no original endpoint or control character was read.
    pub source_free_continuation: bool,
    /// Adjacent context read by the real frontier child, accounted separately from
    /// Source scanning and halo-bearing shaping input.
    #[cfg(feature = "paged-shaping-frontier")]
    pub context_bytes: usize,
    pub scalars: usize,
    pub records: usize,
    pub metadata_units: usize,
    pub shapes: usize,
    pub answer: MarkdownLineComposeAnswer,
}
impl MarkdownLineComposeStep {
    fn yielded() -> Self {
        Self {
            bytes: 0,
            source_free_continuation: false,
            #[cfg(feature = "paged-shaping-frontier")]
            context_bytes: 0,
            scalars: 0,
            records: 0,
            metadata_units: 0,
            shapes: 0,
            answer: MarkdownLineComposeAnswer::Yielded,
        }
    }
}
#[derive(Clone, Copy)]
enum Stage {
    Item,
    Graphemes,
    Style,
    Capture,
    Start,
    Continue,
    Measure,
    Publish,
    Complete,
    Resolution,
    ResolvedCapture,
    Control,
    ControlCapture,
    EmptyPublish,
    TabPending,
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    TableRow,
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    TableGraphemes,
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    TableCapture,
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    TablePublish,
}
/// Grapheme migration serves only real Ready items/controls. A budget yield retains
/// the same waiting owner instead of rebuilding its Job.
#[derive(Clone, Copy)]
enum GraphemeResume {
    Item,
    Control,
}
struct Payload {
    source: MarkdownLayoutTextSource,
    coordinator: MarkdownLineItemCoordinator,
    // Word actions retain only a temporary cursor over original grapheme metadata,
    // without another Source, reader, or text copy.
    #[cfg(feature = "paged-shaping-frontier")]
    word_delete: Option<word::WordJob>,
    store: GlyphStore,
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    table: Option<table_row::PendingRow>,
    stage: Stage,
    // An actual pending line-break query leaves one resume flag. The next quantum
    // advances only its original dependency, preserving the stage and consumer.
    #[cfg(feature = "paged-shaping-frontier")]
    line_breaks_pending: bool,
    #[cfg(feature = "paged-shaping-frontier")]
    graphemes_pending: bool,
    request: Option<MarkdownLineStyleRequest>,
    item: Option<MarkdownLineItem>,
    graphemes: Option<(usize, GraphemeResume)>,
    current: Option<StoredItemProof>,
    next: Option<StoredItemProof>,
    measure: Option<MarkdownSoftLineMeasureJob>,
    next_start: Option<SelectedLineStart>,
    resolution: Option<MarkdownSoftLineMeasureChoice>,
    resolver: Option<MarkdownStoredPrefixResolveJob>,
    resolved_capture: Option<ResolvedCapture>,
    control: Option<MarkdownLineInlineControl>,
    empty_due: bool,
    empty_capture: bool,
    pen: i64,
    next_pen: i64,
    tab_pending: Option<MarkdownLineTabPlacement>,
    start: usize,
    width: i64,
    style_inset_26_6: i32,
    // Actual selectors and transferred prefixes drop before the resolver claim
    // settles their original resource observations.
    resolver_resources: Option<super::worker_resources::ResolveResourceClaim>,
}

impl Payload {
    /// Initial construction and same-Source restyling share this one composition
    /// cursor. The caller has already established ownership of its index dependencies.
    fn new(
        source: MarkdownLayoutTextSource,
        coordinator: MarkdownLineItemCoordinator,
        store: GlyphStore,
        width: i64,
    ) -> Box<Self> {
        Box::new(Self {
            source,
            coordinator,
            #[cfg(feature = "paged-shaping-frontier")]
            word_delete: None,
            store,
            #[cfg(all(
                feature = "s2-table-window-prototype",
                feature = "paged-shaping-frontier"
            ))]
            table: None,
            stage: Stage::Item,
            #[cfg(feature = "paged-shaping-frontier")]
            line_breaks_pending: false,
            #[cfg(feature = "paged-shaping-frontier")]
            graphemes_pending: false,
            request: None,
            item: None,
            graphemes: None,
            current: None,
            next: None,
            measure: None,
            next_start: None,
            resolution: None,
            resolver: None,
            resolved_capture: None,
            control: None,
            empty_due: false,
            empty_capture: false,
            pen: 0,
            next_pen: 0,
            tab_pending: None,
            start: 0,
            width,
            style_inset_26_6: 0,
            resolver_resources: None,
        })
    }
}

/// Retains one actual Job, one Measure, and a fixed-page Store. Historical items are
/// searched in the same anonymous file. The registered palette owns fonts; this
/// worker and final line values retain weak registrations in its bounded catalog.
pub struct MarkdownLineComposer {
    // This worker exclusively owns the fixed-size continuation. Taking/returning
    // its shell moves a pointer, not the inline plan workspace. Allocation stays
    // independent of text length; field drop order and whole-worker failure remain.
    payload: Option<Box<Payload>>,
    authority: MarkdownLayoutTextAuthority,
    cancellation: MarkdownLineComposeCancellation,
    backing: Option<MarkdownOriginalBackingObserver>,
    resources: super::worker_resources::ComposerResourceScope,
}
impl MarkdownLineComposer {
    pub fn new(
        source: MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        fonts: MarkdownLineFontCatalog,
        width_26_6: i64,
        cancellation: MarkdownLineComposeCancellation,
    ) -> Result<Self> {
        // Validate the original Source/cancellation chain before creating the actual
        // coordinator and unique writer, then share their resource observation scope.
        source.health()?;
        if !source.permits(authority) {
            return Err(MarkdownLineComposeError::SourceMismatch);
        }
        if width_26_6 <= 0 {
            return Err(MarkdownLineComposeError::InvalidRequest);
        }
        let coordinator =
            MarkdownLineItemCoordinator::new(source.clone(), authority, cancellation.clone())?;
        let mut store = GlyphStore::new(source.clone(), authority, fonts, cancellation.clone())?;
        let resources = coordinator.resource_scope();
        store.set_resource_scope(resources.clone());
        let backing = source.original_backing_observer();
        Ok(Self {
            payload: Some(Payload::new(source, coordinator, store, width_26_6)),
            authority: authority.clone(),
            cancellation,
            backing,
            resources,
        })
    }
    /// Explicit frontier construction publishes only certified prefixes. A worker
    /// created for complete-item processing cannot change policy after work starts.
    #[cfg(feature = "paged-shaping-frontier")]
    pub fn new_with_frontier(
        source: MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        fonts: MarkdownLineFontCatalog,
        width_26_6: i64,
        cancellation: MarkdownLineComposeCancellation,
    ) -> Result<Self> {
        let mut result = Self::new(source, authority, fonts, width_26_6, cancellation)?;
        result
            .payload
            .as_mut()
            .ok_or(MarkdownLineComposeError::Poisoned)?
            .coordinator
            .enable_frontier()?;
        Ok(result)
    }
    /// Check original Source health before reading fixed work facts; this neither
    /// forces layout nor reads text or renews a producing scope.
    #[cfg(feature = "paged-shaping-frontier")]
    pub fn frontier_work(&self) -> Result<MarkdownLineFrontierWork> {
        self.guard()?;
        self.payload
            .as_ref()
            .ok_or(MarkdownLineComposeError::Poisoned)?
            .coordinator
            .frontier_work()
    }
    fn guard(&self) -> Result<()> {
        // The original backing's first failure wins even after payload retirement;
        // cancellation or an empty continuation must not hide that Source cause.
        if let Some(errno) = self.backing.as_ref().and_then(|value| value.first_errno()) {
            return Err(
                crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into(),
            );
        }
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownLineComposeError::Poisoned)?;
        payload.source.health()?;
        if !payload.source.permits(&self.authority) {
            return Err(MarkdownLineComposeError::SourceMismatch);
        }
        if self.cancellation.is_cancelled() {
            return Err(MarkdownLineComposeError::Cancelled);
        }
        Ok(())
    }
    fn finish<T>(&mut self, result: Result<T>) -> Result<T> {
        // Recheck original authority after actual work. On failure, cancel the same
        // deep selector before dropping all child allocations and the unique writer.
        let result = match self.guard() {
            Err(error) => {
                drop(result);
                Err(error)
            }
            Ok(()) => result,
        };
        if result.is_err() {
            if let Some(resolver) = self
                .payload
                .as_ref()
                .and_then(|value| value.resolver.as_ref())
            {
                resolver.cancellation().cancel();
            }
            drop(self.payload.take());
        } // No failure leaves a reusable partial directory or original Job behind.
        if let Some(errno) = self.backing.as_ref().and_then(|value| value.first_errno()) {
            return Err(
                crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into(),
            );
        }
        result
    }
    pub fn source_authority(&self) -> &MarkdownLayoutTextAuthority {
        &self.authority
    }

    /// Install the original anchors' bounded physical-line projection before the
    /// first step. Text, Source identity, and language indexes stay unchanged; hidden
    /// ranges still cover canonical coordinates without entering font selection.
    ///
    /// # Errors
    /// Revoke this worker on excess folds, non-physical-line endpoints, layout that
    /// has already started, or original Source failure.
    pub fn install_folded_ranges(&mut self, ranges: &[std::ops::Range<usize>]) -> Result<()> {
        let result = (|| {
            self.guard()?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            if !matches!(payload.stage, Stage::Item) || payload.start != 0 {
                return Err(MarkdownLineComposeError::InvalidRequest);
            }
            payload.coordinator.install_folded_ranges(ranges)?;
            // Output shares the same bounded endpoint proof. Queries distinguish an
            // unmigrated hidden range from a known non-grapheme position.
            payload.store.set_fold_plan(payload.coordinator.fold_plan());
            Ok(())
        })();
        self.finish(result)
    }
    /// Observe the original writer's actual file/mapping and Source release. A
    /// snapshot reader must obtain its independent resource observation.
    pub fn resource_observer(&self) -> Result<MarkdownComposedLineResourceObserver> {
        self.guard()?;
        self.payload
            .as_ref()
            .ok_or(MarkdownLineComposeError::Poisoned)?
            .store
            .resource_observer()
    }
    /// Observe original resource facts after failure/cancellation without granting
    /// new text, font, or layout authority.
    pub fn worker_resource_observer(&self) -> MarkdownLineComposerWorkerResourceObserver {
        self.resources.observer()
    }

    /// Selection-sensitive syntax visibility invalidates old glyphs/soft lines while
    /// retaining the same Source and prepared language indexes. The caller must
    /// release old prefix readers/font trials first. This method neither reads text
    /// nor rebuilds UBA, Script, or grapheme metadata.
    ///
    /// # Errors
    /// Revoke this worker on Source failure, cancellation, retained old derived
    /// resources, or failure to create the new output file.
    #[cfg(feature = "paged-shaping-frontier")]
    pub fn reset_styles(&mut self, fonts: MarkdownLineFontCatalog) -> Result<()> {
        let old_resources = self.resources.observer();
        let result = (|| {
            self.guard()?;
            // End the old glyph graph before creating another writer. Destructuring
            // moves reusable indexes out while old Measure/item/Store fields drop.
            let (source, mut coordinator, width, writer) = {
                let mut payload = self
                    .payload
                    .take()
                    .ok_or(MarkdownLineComposeError::Poisoned)?;
                close_resolver(&mut payload)?;
                let writer = payload.store.resource_observer()?;
                let Payload {
                    source,
                    coordinator,
                    store,
                    width,
                    ..
                } = *payload;
                drop(store);
                (source, coordinator, width, writer)
            };
            coordinator.reset_styles()?;
            let closed = writer.snapshot();
            let retired = old_resources
                .snapshot()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            if closed.file_open
                || closed.mapped_bytes != 0
                || closed.cleanup_failed
                || closed.reader_failed
                || !retired.coverage_complete
                || retired.observation_failed
                || retired.shapes.active != 0
                || retired.shapes.cleanup_failed
                || retired.shapes.first_error.is_some()
                || retired.clusters.active != 0
                || retired.clusters.cleanup_failed
                || retired.resolvers.active != 0
                || retired.resolvers.cleanup_failed
            {
                return Err(MarkdownLineComposeError::Poisoned);
            } // A fresh empty writer cannot erase real old cleanup failure. Reused
            // language dependencies are observed separately at final session close.
            let resources = coordinator.resource_scope();
            let mut store = GlyphStore::new(
                source.clone(),
                &self.authority,
                fonts,
                self.cancellation.clone(),
            )?;
            store.set_resource_scope(resources.clone());
            store.set_fold_plan(coordinator.fold_plan());
            #[cfg(feature = "s2-table-window-prototype")]
            store.set_table_plan(coordinator.table_plan());
            self.resources = resources;
            self.payload = Some(Payload::new(source, coordinator, store, width));
            Ok(())
        })();
        if let Err(cause) = result {
            // Release the failed candidate before inspecting the Source/shape first
            // failure that its actual Drop may just have recorded.
            drop(self.payload.take());
            if let Some(errno) = self
                .backing
                .as_ref()
                .and_then(|backing| backing.first_errno())
            {
                return Err(
                    crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into(),
                );
            }
            if let Some(error) = old_resources
                .snapshot()
                .and_then(|resources| resources.shapes.first_error)
            {
                return Err(MarkdownLineComposeError::StyleResetCleanup {
                    cause: Box::new(cause),
                    operation: error.operation,
                    errno: error.errno,
                }); // Preserve both the operation cause and actual cleanup errno.
            }
            // Payload was moved out; ordinary finish would replace this real cause
            // with an empty-payload error rather than preserve the original failure.
            return Err(cause);
        }
        self.finish(Ok(()))
    }
    /// Read the currently issued style request without advancing the coordinator,
    /// reading text, or renewing its nonce. The caller may retain it across turns
    /// and consume it through the same original response entry.
    ///
    /// # Errors
    /// Reject failed Source, cancellation, or retired worker. Missing the original
    /// request in Style remains an actual coverage error.
    pub fn pending_style_request(&self) -> Result<Option<MarkdownLineStyleRequest>> {
        self.guard()?;
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownLineComposeError::Poisoned)?;
        if !matches!(payload.stage, Stage::Style) {
            return Ok(None);
        }
        payload
            .request
            .as_ref()
            .cloned()
            .map(Some)
            .ok_or(MarkdownLineComposeError::InconsistentCoverage)
    }

    /// Borrow the original coordinator's sole dependencies for one grapheme trial,
    /// without advancing layout or creating another index. Responses consume its
    /// original nonce; equal ranges/font names or a recaptured document cannot renew it.
    ///
    /// # Errors
    /// Revoke the complete worker on original Source, stage, request, or cancellation
    /// failure.
    pub fn font_probe(
        &mut self,
        request: &MarkdownLineStyleRequest,
    ) -> Result<MarkdownLineFontProbe> {
        let result = (|| {
            self.guard()?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            if !matches!(payload.stage, Stage::Style) {
                return Err(MarkdownLineComposeError::InvalidRequest);
            }
            payload.coordinator.font_probe(request)
        })();
        self.finish(result)
    }

    /// Check whether the original SourceRequest's style-invariance capability covers
    /// this composer's actual captured inventory. Admission is limited to a line
    /// start just after ordinary complete-line publication; no writer truncation,
    /// output copy, or resource-owner replacement occurs. The capability grants no
    /// grapheme/font authority: Native still retires old Scene/readers and republishes
    /// under complete new input.
    ///
    /// # Errors
    /// Preserve original Source, cancellation, publication, and font-registration
    /// failures. False only means that the current continuation cannot be reused.
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    pub fn can_retain_selection_styles(
        &self,
        capability: &crate::MarkdownSelectionStyleInvariant<'_>,
    ) -> Result<bool> {
        self.guard()?;
        let result = (|| {
            let payload = self
                .payload
                .as_ref()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            let proven_end = capability
                .end_for(&payload.source)
                .map_err(|error| match error {
                    crate::markdown_document_buffer::MarkdownTableRequestError::Source(cause) => {
                        MarkdownLineComposeError::Source(cause)
                    }
                    cause => MarkdownLineComposeError::RootLeafBoundary(Box::new(cause)),
                })?;
            // Only ordinary Publish obtains next_start from an actual selected
            // permit, clears current/measure, and sets start to the latest LineEnd.
            // Resolved-prefix publication remains in Resolution; its retained ranges
            // or an old start ticket cannot establish this quiescent boundary.
            if !matches!(payload.stage, Stage::Start)
                || payload.next_start.is_none()
                || payload.item.is_some()
                || payload.current.is_some()
                || payload.next.is_some()
                || payload.measure.is_some()
                || payload.request.is_some()
                || payload.graphemes.is_some()
                || payload.control.is_some()
                || payload.resolution.is_some()
                || payload.resolver.is_some()
                || payload.resolved_capture.is_some()
                || payload.resolver_resources.is_some()
                || payload.tab_pending.is_some()
                || payload.word_delete.is_some()
                || payload.table.is_some()
                || payload.line_breaks_pending
                || payload.graphemes_pending
                || payload.empty_due
                || payload.empty_capture
            {
                return Ok(false);
            }
            let Some(captured_end) = payload
                .store
                .selection_style_capture_end(payload.start, proven_end)?
            else {
                return Ok(false);
            };
            // Store coverage includes successor items prefetched for Measure; the
            // visible LineEnd alone would miss old-style glyphs. Root Code can prove
            // the same-Source whole domain, while Plain proves only its read prefix.
            // Both still require original consumed-end/support/font checks below.
            payload
                .coordinator
                .retains_selection_styles(captured_end, proven_end)
        })();
        self.guard()?;
        result
    }

    /// After the actual font candidate releases its worker, advance the same
    /// grapheme's next level segment through the sole coordinator. Transfer only
    /// actual metadata work, without another bidi/Script index or platform owner.
    pub fn step_font_probe_segment(
        &mut self,
        probe: &mut MarkdownLineFontProbe,
        budget: MarkdownLineComposeBudget,
    ) -> Result<MarkdownLineFontProbeSegmentStep> {
        let result = (|| {
            self.guard()?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            if !matches!(payload.stage, Stage::Style) {
                return Err(MarkdownLineComposeError::InvalidRequest);
            }
            payload.coordinator.step_font_probe_segment(probe, budget)
        })();
        self.finish(result)
    }

    /// Restart the enclosing grapheme only after this candidate misses a real
    /// segment. Original request, cancellation, and failure remain this composer's.
    pub fn restart_font_probe(&mut self, probe: &mut MarkdownLineFontProbe) -> Result<()> {
        let result = (|| {
            self.guard()?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            if !matches!(payload.stage, Stage::Style) {
                return Err(MarkdownLineComposeError::InvalidRequest);
            }
            payload.coordinator.restart_font_probe(probe)
        })();
        self.finish(result)
    }

    /// Borrow the same RootLeaf ticket for the current right-grapheme request;
    /// never put its Rc publication in Send requests/font workers. A color/style
    /// change alone grants no closure; the sole coordinator checks the actual font.
    ///
    /// # Errors
    /// Reject original Source failure, cancellation, invalid stage, or stale nonce;
    /// an identical numeric range cannot replay the ticket.
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    pub fn root_leaf_style_boundary_for(
        &self,
        request: &MarkdownLineStyleRequest,
    ) -> Result<Option<&crate::VerifiedMarkdownRootLeafStyleBoundary>> {
        self.guard()?;
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownLineComposeError::Poisoned)?;
        if !matches!(payload.stage, Stage::Style) {
            return Err(MarkdownLineComposeError::InvalidRequest);
        }
        payload.coordinator.root_leaf_style_boundary_for(request)
    }

    /// Consume the original coordinator nonce and let that same coordinator validate
    /// the complete font response before resuming item work.
    pub fn provide_style(&mut self, response: MarkdownLineStyleResponse) -> Result<()> {
        let result = (|| {
            self.guard()?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            if !matches!(payload.stage, Stage::Style) {
                return Err(MarkdownLineComposeError::InvalidRequest);
            }
            let inset = response.paragraph_inset_26_6;
            #[cfg(all(
                feature = "s2-table-window-prototype",
                feature = "paged-shaping-frontier"
            ))]
            let font_witness = payload
                .coordinator
                .root_leaf_style_boundary_for(
                    payload
                        .request
                        .as_ref()
                        .ok_or(MarkdownLineComposeError::InvalidRequest)?,
                )?
                .is_some();
            #[cfg(not(all(
                feature = "s2-table-window-prototype",
                feature = "paged-shaping-frontier"
            )))]
            let font_witness = false;
            #[cfg(feature = "paged-shaping-frontier")]
            {
                if payload
                    .request
                    .as_ref()
                    .is_some_and(|request| request.continuation_font().is_some())
                {
                    // This font-transfer grapheme already has two charged endpoint
                    // proofs; consuming its response needs no index query/text read.
                    payload.coordinator.provide_style(response)?;
                } else {
                    payload.coordinator.queue_style(response)?;
                }
            }
            #[cfg(not(feature = "paged-shaping-frontier"))]
            payload.coordinator.provide_style(response)?;
            if !font_witness {
                payload.style_inset_26_6 = inset;
            } // A right-grapheme witness cannot overwrite the pending left item's
            // paragraph inset. Ordinary responses retain the original contract.
            payload.request = None;
            payload.stage = Stage::Item;
            Ok(())
        })();
        self.finish(result)
    }
    pub fn provide_control(&mut self, response: MarkdownLineControlResponse) -> Result<()> {
        let result = (|| {
            self.guard()?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            let request = payload
                .control
                .as_ref()
                .ok_or(MarkdownLineComposeError::InvalidRequest)?;
            if !matches!(payload.stage, Stage::Control) || !request.accepts(&response) {
                return Err(MarkdownLineComposeError::SourceMismatch);
            }
            // Match the one pending control before deriving empty-paragraph facts.
            // A trailing line separator retains its real UBA base; other EOF empties
            // follow the existing empty-paragraph policy.
            payload.empty_capture = request.kind == MarkdownLineInlineControlKind::EmptyParagraph;
            let value = if payload.empty_capture {
                let end = payload.source.utf16_len();
                let level = if end > 0
                    && payload
                        .source
                        .snapshot_for_bidi()
                        .utf16_code_unit_at(end - 1)?
                        == 0x2028
                {
                    let (bidi, _) = payload.coordinator.dependencies()?;
                    bidi.paragraph_base_level_at_utf16(end - 1)?
                        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?
                } else {
                    0
                };
                super::controls::ValidatedControl::empty(request, response, &payload.source, level)?
            } else {
                payload.coordinator.consume_control(response)?
            };
            let at_eof = value.facts.source_utf16.end == payload.source.utf16_len();
            let trailing_separator =
                if at_eof && value.request.kind == MarkdownLineInlineControlKind::FoldedSection {
                    // Preserve the original trailing empty paragraph after a real
                    // separator. A fold ending at unterminated text EOF adds none.
                    matches!(
                        payload
                            .source
                            .snapshot_for_bidi()
                            .utf16_code_unit_at(value.facts.source_utf16.end - 1)?,
                        10 | 13 | 0x85 | 0x2028 | 0x2029
                    )
                } else {
                    true
                };
            payload.empty_due = !payload.empty_capture
                && value.request.kind != MarkdownLineInlineControlKind::Tab
                && at_eof
                && trailing_separator;
            payload.store.begin_control(value)?;
            payload.control = None;
            payload.stage = Stage::ControlCapture;
            Ok(())
        })();
        self.finish(result)
    }

    /// Advance one kind of bounded work per turn. UBA remains an explicitly
    /// cancellable Source-wide metadata phase, not a whole-text render copy.
    pub fn step(&mut self, budget: MarkdownLineComposeBudget) -> Result<MarkdownLineComposeStep> {
        let result = self.advance(budget);
        self.finish(result)
    }
    fn advance(&mut self, budget: MarkdownLineComposeBudget) -> Result<MarkdownLineComposeStep> {
        self.guard()?;
        if budget.bytes == 0
            || budget.scalars == 0
            || budget.records == 0
            || budget.metadata_units == 0
        {
            return Err(MarkdownLineComposeError::InvalidRequest);
        }
        let mut result = MarkdownLineComposeStep::yielded();
        let payload = self
            .payload
            .as_mut()
            .ok_or(MarkdownLineComposeError::Poisoned)?;
        #[cfg(feature = "paged-shaping-frontier")]
        if payload.line_breaks_pending {
            // The waiting consumer already spent its previous quota. Advance only
            // the original line-break dependency now; retrying the consumer here
            // would spend the same byte/record allowance twice.
            let progress = payload.coordinator.advance_pending_line_breaks(budget)?;
            payload.line_breaks_pending = false;
            result.bytes = progress.bytes;
            result.scalars = progress.scalars;
            result.records = progress.records;
            return Ok(result);
        }
        #[cfg(feature = "paged-shaping-frontier")]
        if payload.graphemes_pending {
            let progress = payload.coordinator.advance_pending_graphemes(budget)?;
            payload.graphemes_pending = false;
            result.bytes = progress.bytes;
            result.scalars = progress.scalars;
            result.records = progress.records;
            return Ok(result); // Resume the consumer later under its own next quota.
        }
        // Only audited grapheme/glyph directory phases qualify as Source-free.
        // Item, line-start, and resolved publication may read Source endpoints even
        // with zero reported bytes. Dependency early returns stay false; their
        // actual reads keep their original separate step and failure boundary.
        result.source_free_continuation = matches!(
            payload.stage,
            Stage::Graphemes | Stage::Capture | Stage::Measure | Stage::ControlCapture
        );
        match payload.stage {
            #[cfg(all(
                feature = "s2-table-window-prototype",
                feature = "paged-shaping-frontier"
            ))]
            Stage::TableRow | Stage::TableGraphemes | Stage::TableCapture | Stage::TablePublish => {
                table_row::advance(payload, &self.authority, budget, &mut result)?;
            }

            Stage::Item => {
                let progress = payload.coordinator.step(budget)?;
                result.bytes = progress.bytes;
                #[cfg(feature = "paged-shaping-frontier")]
                {
                    result.context_bytes = progress.context_bytes;
                }
                result.scalars = progress.scalars;
                result.records = progress.records;
                result.metadata_units = progress.metadata_units;
                result.shapes = progress.shapes;
                match progress.answer {
                    MarkdownLineItemAnswer::Yielded => {}
                    MarkdownLineItemAnswer::NeedStyle(request) => {
                        payload.request = Some(request);
                        // Style trials consume the original Break Job without
                        // copying every Source grapheme into output. Migrate only
                        // the bounded prefix needed by an actual Ready child.
                        payload.stage = Stage::Style;
                    }
                    MarkdownLineItemAnswer::ItemReady(_) => {
                        let item = payload.coordinator.take_item()?;
                        let end = item.facts().source_utf16.end;
                        payload.item = Some(item);
                        payload.graphemes = Some((end, GraphemeResume::Item));
                        payload.stage = Stage::Graphemes;
                    }
                    MarkdownLineItemAnswer::NeedsControl(control) => {
                        payload.graphemes =
                            Some((control.source_utf16.end, GraphemeResume::Control));
                        payload.control = Some(control);
                        payload.stage = Stage::Graphemes;
                    }
                    #[cfg(feature = "paged-shaping-frontier")]
                    MarkdownLineItemAnswer::NeedsFrontier(reason) => {
                        result.answer = MarkdownLineComposeAnswer::NeedsFrontier(reason);
                    }
                    MarkdownLineItemAnswer::EndOfSource => {
                        if payload.start != payload.source.utf16_len() {
                            return Err(MarkdownLineComposeError::InconsistentCoverage);
                        }
                        if payload.source.utf16_len() == 0 {
                            payload.control = Some(MarkdownLineInlineControl::new(
                                self.authority.clone(),
                                0..0,
                                MarkdownLineInlineControlKind::EmptyParagraph,
                            ));
                            payload.graphemes = Some((0, GraphemeResume::Control));
                            payload.stage = Stage::Graphemes;
                        } else {
                            payload.stage = Stage::Complete;
                            result.answer = MarkdownLineComposeAnswer::Complete;
                        }
                    }
                }
            }
            Stage::Graphemes => {
                let (through, resume) = payload
                    .graphemes
                    .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                let (_, breaks) = payload.coordinator.dependencies()?;
                let progress = match resume {
                    GraphemeResume::Item => payload.store.capture_item_graphemes(
                        breaks,
                        payload
                            .item
                            .as_ref()
                            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?,
                        budget.records.min(256),
                        budget.metadata_units.min(256),
                    )?,
                    GraphemeResume::Control => payload.store.capture_graphemes_through(
                        breaks,
                        through,
                        budget.records.min(256),
                        budget.metadata_units.min(256),
                    )?,
                };
                result.records = progress.records;
                result.metadata_units = progress.metadata_units;
                if progress.ready {
                    payload.graphemes = None;
                    // Start glyph/control capture after required graphemes are
                    // actually written. Interleaved record kinds keep each original
                    // glyph/cluster/style segment contiguous and producer-authenticated.
                    payload.stage = match resume {
                        GraphemeResume::Item => {
                            payload.store.begin_with_inset(
                                payload
                                    .item
                                    .as_mut()
                                    .ok_or(MarkdownLineComposeError::InconsistentCoverage)?,
                                payload.style_inset_26_6,
                            )?;
                            Stage::Capture
                        }
                        GraphemeResume::Control => Stage::Control,
                    };
                }
            }
            Stage::Style => {
                result.answer = MarkdownLineComposeAnswer::NeedStyle(
                    payload
                        .request
                        .as_ref()
                        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?
                        .clone(),
                );
            }
            Stage::Capture => {
                let before = payload.store.record_count()?;
                if let Some(proof) = payload.store.step(
                    payload
                        .item
                        .as_mut()
                        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?,
                    budget.records.min(256),
                )? {
                    // Release the actual Job/FD only after complete glyph, cluster,
                    // style, and opaque origin records have been committed.
                    drop(payload.item.take());
                    if payload.measure.is_some() {
                        payload.next = Some(proof);
                        payload.stage = Stage::Continue;
                    } else {
                        payload.current = Some(proof);
                        payload.stage = Stage::Start;
                    }
                }
                result.records = payload
                    .store
                    .record_count()?
                    .checked_sub(before)
                    .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
            }
            Stage::Start => {
                // The next line starts either from committed Store coverage or
                // newly prepared original items. A real EOF may still owe its
                // trailing empty-paragraph control; a prefix end never implies EOF.
                if payload.start == payload.source.utf16_len() {
                    if payload.empty_due {
                        payload.empty_due = false;
                        payload.control = Some(MarkdownLineInlineControl::new(
                            self.authority.clone(),
                            payload.start..payload.start,
                            MarkdownLineInlineControlKind::EmptyParagraph,
                        ));
                        payload.graphemes = Some((payload.start, GraphemeResume::Control));
                        payload.stage = Stage::Graphemes;
                    } else {
                        payload.stage = Stage::Complete;
                        result.answer = MarkdownLineComposeAnswer::Complete;
                    }
                } else if let Some(located) = payload.store.item_at(payload.start)? {
                    let proof = located.proof;
                    if let Some(placement) = payload.store.tab_placement(&proof, 0)?
                        && !matches!(placement, MarkdownLineTabPlacement::Left { .. })
                    {
                        payload.tab_pending = Some(placement);
                        payload.stage = Stage::TabPending;
                        return Ok(result);
                    }
                    let facts = payload.store.facts(&proof)?;
                    let font = facts.registration.authority()?;
                    let width = payload
                        .width
                        .checked_sub(i64::from(facts.paragraph_inset_26_6) * 2)
                        .filter(|width| *width > 0)
                        .ok_or(MarkdownLineComposeError::InvalidRequest)?;
                    let (_, breaks) = payload.coordinator.dependencies()?;
                    let mut provider = payload.store.borrow_at(&proof, 0)?;
                    let request = MarkdownSoftLineMeasureRequest {
                        start_utf16: payload.start,
                        width_26_6: width,
                        cancellation: self.cancellation.measure(),
                    };
                    payload.measure = Some(if let Some(start) = payload.next_start.take() {
                        MarkdownSoftLineMeasureJob::after_selected_start(
                            payload.source.clone(),
                            &self.authority,
                            &font,
                            &mut provider,
                            breaks,
                            request,
                            start,
                        )?
                    } else {
                        MarkdownSoftLineMeasureJob::new_with_provider(
                            payload.source.clone(),
                            &self.authority,
                            &font,
                            &mut provider,
                            breaks,
                            request,
                        )?
                    }); // Only the latest private LineEnd ticket admits an emergency
                    // grapheme start; ordinary construction still needs real UAX evidence.
                    payload.current = Some(proof);
                    payload.pen = 0;
                    payload.stage = Stage::Measure;
                } else {
                    payload.stage = Stage::Item;
                }
            }
            Stage::Continue => {
                let old = payload
                    .current
                    .as_ref()
                    .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                let next = payload
                    .next
                    .take()
                    .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                let placement = payload.store.tab_placement(&next, payload.next_pen)?;
                // Charge one semantic admission of the sealed next item, not an
                // invented count of low-level preads. An unresolved tab still takes
                // its original control path; reading a directory does not admit it.
                result.metadata_units = 1;
                if let Some(placement) = placement
                    && !matches!(placement, MarkdownLineTabPlacement::Left { .. })
                {
                    payload.next = Some(next);
                    payload.tab_pending = Some(placement);
                    payload.stage = Stage::TabPending;
                    return Ok(result);
                }
                let font = payload.store.facts(&next)?.registration.authority()?;
                let measure = payload
                    .measure
                    .as_mut()
                    .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                let continuation = {
                    let mut provider = payload.store.borrow_at(old, payload.pen)?;
                    measure.prepare_continue(&mut provider)?
                };
                let (_, breaks) = payload.coordinator.dependencies()?;
                {
                    let mut provider = payload.store.borrow_at(&next, payload.next_pen)?;
                    continuation.continue_with(&mut provider, &font, breaks)?;
                }
                payload.current = Some(next);
                payload.pen = payload.next_pen;
                payload.stage = Stage::Measure;
                // The same Store's actual item handoff has completed; only existing
                // directories/glyphs were consumed. Earlier failure/returns grant none.
                result.source_free_continuation = true;
            }
            Stage::Measure => {
                let current = payload
                    .current
                    .as_ref()
                    .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                let (_, breaks) = payload.coordinator.dependencies()?;
                let progress = {
                    let mut provider = payload.store.borrow_at(current, payload.pen)?;
                    payload
                        .measure
                        .as_mut()
                        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?
                        .step_with_provider(
                            &mut provider,
                            breaks,
                            MarkdownSoftLineMeasureBudget {
                                break_queries: budget.metadata_units.min(64),
                                clusters: budget.records.min(256),
                            },
                        )?
                }; // End the exclusive Store borrow before locating/writing a successor.
                result.records = progress.clusters;
                result.metadata_units = progress.break_queries;
                if let MarkdownSoftLineMeasureAnswer::Selected(choice) = progress.answer {
                    match choice {
                        MarkdownSoftLineMeasureChoice::MeasuredBreak(_) => {
                            payload.stage = Stage::Publish
                        }
                        #[cfg(feature = "paged-shaping-frontier")]
                        MarkdownSoftLineMeasureChoice::MeasuredEmergencyBreak { .. } => {
                            payload.stage = Stage::Publish
                        } // Reuse measured glyphs only under the original sealed
                        // Measure permit, never a public emergency range alone.
                        MarkdownSoftLineMeasureChoice::NeedNextItem {
                            source_utf16,
                            x_advance_26_6,
                            ..
                        } => {
                            payload.next_pen = x_advance_26_6;
                            if let Some(next) = payload.store.item_at(source_utf16.end)? {
                                payload.next = Some(next.proof);
                                payload.stage = Stage::Continue;
                            } else {
                                payload.stage = Stage::Item;
                            }
                        }
                        blocked @ (MarkdownSoftLineMeasureChoice::NeedsFontFallback { .. }
                        | MarkdownSoftLineMeasureChoice::NeedsDictionary(_)) => {
                            // Missing-font/dictionary requests have no reshape
                            // glyph permit yet. Keep the original selected Measure
                            // state and recheck Source/font on resume; do not enter
                            // the reshape-only resolver or disguise InvalidRequest
                            // as fallback handling or a completed soft line.
                            result.answer = MarkdownLineComposeAnswer::NeedsResolution(blocked);
                        }
                        other => {
                            payload.resolution = Some(other);
                            payload.stage = Stage::Resolution;
                        }
                    }
                }
            }
            Stage::Publish => {
                let current = payload
                    .current
                    .as_ref()
                    .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                let measure = payload
                    .measure
                    .as_ref()
                    .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                let permit = {
                    let mut provider = payload.store.borrow_at(current, payload.pen)?;
                    measure.selected_permit(&mut provider)?
                };
                let (bidi, _) = payload.coordinator.dependencies()?;
                let line =
                    payload
                        .store
                        .append_selected_line(current, &permit, bidi, payload.pen)?;
                let next_start = {
                    let mut provider = payload.store.borrow_at(current, payload.pen)?;
                    permit.next_start(&mut provider)?
                }; // Issue a single-use start only after real LineEnd commit; a
                // failed publication cannot carry a partial line into next Measure.
                payload.next_start = Some(next_start);
                payload.start = line.source_utf16.end;
                payload.measure = None;
                payload.current = None;
                payload.stage = Stage::Start;
                result.answer = MarkdownLineComposeAnswer::LineReady(line);
            }
            Stage::Complete => result.answer = MarkdownLineComposeAnswer::Complete,
            Stage::Resolution => {
                if payload.resolver.is_none() {
                    let selected = match payload.resolution.as_ref() {
                        Some(MarkdownSoftLineMeasureChoice::NeedsEmergencyBreak(value)) => {
                            Some(value.source_utf16.clone())
                        }
                        Some(MarkdownSoftLineMeasureChoice::NeedsReshape {
                            source_utf16, ..
                        }) => Some(source_utf16.clone()),
                        #[cfg(feature = "paged-shaping-frontier")]
                        Some(MarkdownSoftLineMeasureChoice::NeedsFrontierEmergencyPrefix {
                            source_utf16,
                            ..
                        }) => Some(source_utf16.clone()),
                        _ => None,
                    };
                    if let Some(selected) = selected {
                        let current = payload
                            .current
                            .as_ref()
                            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                        let measure = payload
                            .measure
                            .as_mut()
                            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                        let item = {
                            let mut provider = payload.store.borrow_at(current, payload.pen)?;
                            let permit = measure.resolution_permit(&mut provider)?;
                            permit.seed(&mut provider)?.item
                        }; // Authenticate current nonce/selection before releasing
                        // that borrow to locate an earlier same-Store directory.
                        if selected.start < item.start
                            && let Some(previous) = payload.store.resolution_item_at(&selected)?
                        {
                            let previous_font =
                                payload.store.facts(&previous)?.registration.authority()?;
                            let mut provider = payload.store.borrow_at(&previous, 0)?;
                            let facts = crate::markdown_soft_line_measure::provider::GlyphProvider::require(
                                &mut provider, &self.authority, &previous_font,
                            )?;
                            if selected.end <= facts.source_utf16.end {
                                // Restore only when this one original item contains
                                // the complete selected range. Its real registration
                                // and style provide the actual font and point size.
                                measure.restore_selected_item(&mut provider, &previous_font)?;
                                payload.current = Some(previous);
                                payload.pen = 0; // Glyph clusters/reshape ignore pen;
                                // tabs depend on it and were excluded above.
                            } // Cross-font/Script selections retain the original
                            // NextItem exit rather than borrow one fragment as all.
                        }
                    }
                } // Keep the existing selector's anchor; do not rebind/rebuild it each quantum.
                let current = payload
                    .current
                    .as_ref()
                    .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                let measure = payload
                    .measure
                    .as_ref()
                    .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                let permit = {
                    let mut provider = payload.store.borrow_at(current, payload.pen)?;
                    measure.resolution_permit(&mut provider)?
                };
                let (_, breaks) = payload.coordinator.dependencies()?;
                let progress = {
                    let mut provider = payload.store.borrow_at(current, payload.pen)?;
                    if payload.resolver.is_none() {
                        let resolver = MarkdownStoredPrefixResolveJob::new(&permit, &mut provider)?;
                        payload.resolver_resources =
                            Some(self.resources.claim_resolver(resolver.resource_observer()));
                        payload.resolver = Some(resolver);
                    }
                    payload
                        .resolver
                        .as_mut()
                        .ok_or(MarkdownLineComposeError::Poisoned)?
                        .step(
                            &permit,
                            &mut provider,
                            breaks,
                            MarkdownSoftLineResolveBudget {
                                input_bytes: budget.bytes,
                                input_scalars: budget.scalars,
                                glyphs: budget.records.min(256),
                                candidates: budget.metadata_units.min(64),
                            },
                        )?
                };
                result.bytes = progress.input_bytes;
                // Reshaped center input and constructed halo are separately charged;
                // context-only steps must not be described as zero Source work.
                result.scalars = progress.input_scalars;
                #[cfg(feature = "paged-shaping-frontier")]
                {
                    result.context_bytes = progress.context_bytes;
                }
                result.records = progress.glyphs;
                result.metadata_units = progress.candidates;
                result.shapes = progress.shapes;
                match progress.answer {
                    MarkdownEmergencyGlyphSliceAnswer::Yielded
                    | MarkdownEmergencyGlyphSliceAnswer::PendingDependencies => {}
                    MarkdownEmergencyGlyphSliceAnswer::ShapedPrefix(_) => {
                        let prefix = {
                            let mut provider = payload.store.borrow_at(current, payload.pen)?;
                            payload
                                .resolver
                                .as_mut()
                                .ok_or(MarkdownLineComposeError::Poisoned)?
                                .take_signed_prefix(&permit, &mut provider)?
                        };
                        let (bidi, _) = payload.coordinator.dependencies()?;
                        payload.resolved_capture = Some(ResolvedCapture::new(
                            &mut payload.store,
                            current,
                            &permit,
                            prefix,
                            bidi,
                            payload.pen,
                        )?);
                        payload.stage = Stage::ResolvedCapture;
                    }
                    MarkdownEmergencyGlyphSliceAnswer::PrefixComplete {
                        source_utf16_end,
                        source_eof,
                    } => {
                        if source_utf16_end != payload.start
                            || (source_eof && source_utf16_end != payload.source.utf16_len())
                        {
                            return Err(MarkdownLineComposeError::InconsistentCoverage);
                        }
                        close_resolver(payload)?;
                        payload.measure = None;
                        payload.current = None;
                        payload.resolution = None;
                        payload.stage = Stage::Start;
                    }
                    MarkdownEmergencyGlyphSliceAnswer::Pending(_) => {
                        // Dictionary, fallback, or cross-item reshaping still needs
                        // real successors; the current font cannot bless a past item.
                        result.answer = MarkdownLineComposeAnswer::NeedsResolution(
                            payload
                                .resolution
                                .as_ref()
                                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?
                                .clone(),
                        );
                    }
                    MarkdownEmergencyGlyphSliceAnswer::PendingPrefixConsumer
                    | MarkdownEmergencyGlyphSliceAnswer::ReusableLine(_)
                    | MarkdownEmergencyGlyphSliceAnswer::Reshaped(_) => {
                        return Err(MarkdownLineComposeError::InconsistentCoverage);
                    } // This path consumes only real signed prefixes, never an
                      // unreturned child or an earlier complete-tail result.
                }
            }
            Stage::ResolvedCapture => {
                let current = payload
                    .current
                    .as_ref()
                    .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                let measure = payload
                    .measure
                    .as_ref()
                    .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                let permit = {
                    let mut provider = payload.store.borrow_at(current, payload.pen)?;
                    measure.resolution_permit(&mut provider)?
                };
                let before = payload.store.record_count()?;
                let line = payload
                    .resolved_capture
                    .as_mut()
                    .ok_or(MarkdownLineComposeError::Poisoned)?
                    .step(
                        &mut payload.store,
                        current,
                        &permit,
                        payload.pen,
                        budget.records.min(256),
                    )?;
                result.records = payload
                    .store
                    .record_count()?
                    .checked_sub(before)
                    .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                if let Some(line) = line {
                    drop(payload.resolved_capture.take());
                    payload.start = line.source_utf16.end;
                    payload.stage = Stage::Resolution;
                    result.answer = MarkdownLineComposeAnswer::LineReady(line);
                } // Continue the same Measure/selector FIFO without reshaping the full suffix.
            }
            Stage::ControlCapture => {
                let before = payload.store.record_count()?;
                if let Some(proof) = payload.store.step_control(budget.records.min(256))? {
                    if payload.empty_capture {
                        payload.current = Some(proof);
                        payload.stage = Stage::EmptyPublish;
                    } else if payload.measure.is_some() {
                        payload.next = Some(proof);
                        payload.stage = Stage::Continue;
                    } else {
                        payload.current = Some(proof);
                        payload.stage = Stage::Start;
                    }
                }
                result.records = payload
                    .store
                    .record_count()?
                    .checked_sub(before)
                    .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
            }
            Stage::EmptyPublish => {
                let proof = payload
                    .current
                    .as_ref()
                    .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                let facts = payload.store.append_empty_line(proof)?;
                payload.current = None;
                payload.empty_capture = false;
                payload.stage = Stage::Complete;
                result.answer = MarkdownLineComposeAnswer::LineReady(facts);
            }
            Stage::TabPending => {
                result.answer = MarkdownLineComposeAnswer::NeedsTabPlacement(
                    payload
                        .tab_pending
                        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?,
                );
            }
            Stage::Control => {
                result.answer = MarkdownLineComposeAnswer::NeedsControl(
                    payload
                        .control
                        .as_ref()
                        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?
                        .clone(),
                )
            }
        }
        #[cfg(feature = "paged-shaping-frontier")]
        if payload.coordinator.take_line_break_demand()? {
            if !matches!(result.answer, MarkdownLineComposeAnswer::Yielded) {
                return Err(MarkdownLineComposeError::InconsistentCoverage);
            }
            // Only an original line-break query leaves this demand. Other pending
            // glyph/grapheme/fallback/dictionary results keep their own blocked
            // routes and cannot reinterpret an incomplete prefix as EOF.
            payload.line_breaks_pending = true;
        }
        #[cfg(feature = "paged-shaping-frontier")]
        if payload.coordinator.take_grapheme_demand()? {
            if !matches!(result.answer, MarkdownLineComposeAnswer::Yielded) {
                return Err(MarkdownLineComposeError::InconsistentCoverage);
            }
            payload.graphemes_pending = true;
        }
        Ok(result)
    }
    /// Publish actually committed lines while retaining the original worker. Later
    /// scrolling continues the same cursor instead of composing again from Source start.
    #[cfg(feature = "paged-shaping-frontier")]
    pub fn publish_ready_prefix(&mut self) -> Result<Option<MarkdownComposedLines>> {
        let result = (|| {
            self.guard()?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            if payload.resolved_capture.is_some() {
                return Ok(None);
            } // Do not cut a candidate while signed-prefix conversion owns its child.
            payload.store.publish_ready_prefix()
        })();
        if matches!(result, Err(MarkdownLineComposeError::SnapshotBusy)) {
            // Current and candidate readers impose backpressure on a third FD;
            // cancellation or original Source failure still revokes the worker.
            if let Err(error) = self.guard() {
                return self.finish(Err(error));
            }
            return result;
        }
        self.finish(result)
    }

    /// Consume the worker to deliver its committed prefix only. Missing LineEnd or
    /// an in-progress item cannot create a fabricated empty line.
    pub fn into_output(mut self) -> Result<MarkdownComposedLines> {
        self.guard()?;
        let mut payload = self
            .payload
            .take()
            .ok_or(MarkdownLineComposeError::Poisoned)?;
        if payload.resolved_capture.is_some() {
            return Err(MarkdownLineComposeError::InvalidRequest);
        } // The in-progress conversion has no committed LineEnd to publish.
        close_resolver(&mut payload)?;
        let Payload {
            store,
            coordinator,
            item,
            measure,
            ..
        } = *payload;
        drop((coordinator, item, measure));
        store.into_output()
    }
}

fn close_resolver(payload: &mut Payload) -> Result<()> {
    // Explicitly close the real selector and verify its actual jobs before settling
    // its observation claim. The observer itself does not retain the business owner.
    if let Some(resolver) = payload.resolver.take() {
        let observer = resolver.resource_observer();
        let resources = resolver.close()?;
        if resources.active_jobs != 0
            || resources.cleanup_failed
            || observer.snapshot().active_jobs != 0
        {
            return Err(MarkdownLineComposeError::Poisoned);
        }
    }
    drop(payload.resolver_resources.take());
    Ok(())
}

#[cfg(all(test, feature = "paged-shaping-frontier"))]
#[path = "engine/line_prefix_tests.rs"]
mod line_prefix_tests;

#[cfg(all(test, feature = "paged-shaping-frontier"))]
#[path = "engine/unicode_prefix_tests.rs"]
mod unicode_prefix_tests;
