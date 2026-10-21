//! One bounded original Composer continuation keeps prefix directories in anonymous storage without
//! caching whole-document lines or glyphs.

use std::{
    cell::RefCell,
    collections::VecDeque,
    rc::Rc,
    sync::{Arc, mpsc},
    thread::JoinHandle,
};

use dispatch2::MainThreadBound;
use objc2_foundation::{MainThreadMarker, NSPoint};
use omni_markdown_core::{
    MarkdownComposedLine, MarkdownComposedLineFacts, MarkdownComposedLineLookup,
    MarkdownComposedLineLookupAnswer, MarkdownComposedLineLookupTarget, MarkdownComposedLines,
    MarkdownLayoutTextSource, MarkdownLineComposeAnswer, MarkdownLineComposeBudget,
    MarkdownLineComposeCancellation, MarkdownLineComposer, MarkdownLineFontCatalog,
    MarkdownLineFontProbe, MarkdownLineFontProbeSegmentAnswer, MarkdownLineFontProbeSegmentStep,
    MarkdownLineStyleRequest,
};

use super::preparation::PreparationTicket;
use super::{
    NativeMarkdownPagedBody as Owner, NativeMarkdownPagedBodyError as Error,
    NativeMarkdownPagedBodyProgress, NativeMarkdownPagedBodyResourceReceipts,
    NativeMarkdownPagedSceneKey, NativeMarkdownPagedSceneRequest,
    NativeMarkdownPagedTextViewDelegate, Result, Scene, ViewError,
};
use crate::native_surface::{
    paged_font_cascade::NativeMarkdownFontCascadeBudget,
    paged_font_fallback::{
        NativeMarkdownFontChoice, NativeMarkdownFontFallback, NativeMarkdownFontFallbackCompletion,
        NativeMarkdownFontFallbackStep, NativeMarkdownFontTrial, NativeMarkdownFontTrialResult,
        NativeMarkdownProbedFontFallbackRequest,
    },
    paged_line_viewport::{
        NativeMarkdownComposedViewport, NativeMarkdownLineAlignmentMeasure,
        NativeMarkdownMeasuredLine,
    },
    paged_text_view::NativeMarkdownPagedSceneNonce,
};

const METADATA_PER_TURN: usize = 32;
// Trailing Source/grapheme queries share one actual quantum with part metrics; crossing lines grants no
// fresh byte/record allowance.
const LINE_METRIC_BUDGET: MarkdownLineComposeBudget = MarkdownLineComposeBudget {
    bytes: 4096,
    scalars: 256,
    records: 256,
    metadata_units: METADATA_PER_TURN,
};
const MAX_WINDOW_LINES: usize = 256;
// Distant targets defer checkpointing for at most 32 actual LineEnds without changing Core per-step
// read/scalar/metadata budgets.
const MAX_TARGET_BATCH_LINES: usize = 32;

#[path = "actual_y.rs"]
mod actual_y;
use actual_y::{ActualTarget, MeasuredLineAnchor, TargetAdvance, VerticalTarget};
mod cell_word;
mod grid;
use grid::{Grid, TableWidths};

/// Add concrete fields in the same original semantic ledger, keeping processed bytes distinct from
/// allocated workspace capacity.
fn add_semantic_work(
    total: &mut omni_markdown_core::markdown_document_buffer::SemanticWork,
    work: omni_markdown_core::markdown_document_buffer::SemanticWork,
) -> Result<()> {
    for (total, value) in [
        (&mut total.source_bytes, work.source_bytes),
        (&mut total.copied_source_bytes, work.copied_source_bytes),
        (&mut total.metadata_bytes, work.metadata_bytes),
        (&mut total.utf8_validation_bytes, work.utf8_validation_bytes),
        (&mut total.source_tree_steps, work.source_tree_steps),
        (
            &mut total.structural_transitions,
            work.structural_transitions,
        ),
        (
            &mut total.allocated_source_capacity,
            work.allocated_source_capacity,
        ),
        (
            &mut total.normalizer_input_bytes,
            work.normalizer_input_bytes,
        ),
        (&mut total.parser_input_bytes, work.parser_input_bytes),
    ] {
        *total = total.checked_add(value).ok_or(Error::CounterExhausted)?;
    }
    Ok(())
}

/// Preserve both preparation and actual retirement failures; promote only actual Source storage errno
/// to the primary error.
/// Handle this Body concrete typed causes only, without inferring disk failure from text or ordinary
/// cancellation.
pub(super) fn merge_preparation_cleanup(primary: Result<()>, cleanup: Result<()>) -> Result<()> {
    match (primary, cleanup) {
        // If one operation succeeds, retain the other complete result; construct a compound failure
        // only for two actual errors.
        (Ok(()), result) | (result, Ok(())) => result,
        (Err(cause), Err(cleanup)) => {
            let (cause, cleanup) = if source_io(&cleanup) && !source_io(&cause) {
                (cleanup, cause) // A first Source errno observed during cleanup takes priority while preserving the original operation as the secondary cause.
            } else {
                (cause, cleanup) // Preserve operation order for ordinary failures or an existing primary Source cause.
            };
            Err(Error::PreparationCleanup {
                cause: Box::new(cause),
                cleanup: Box::new(cleanup),
            })
        }
    }
}

/// Recognize existing typed storage-errno chains on this retirement path only; authority revocation
/// cannot change primary-error priority.
fn source_io(error: &Error) -> bool {
    use omni_markdown_core::{
        MarkdownDocumentBufferError as Document, MarkdownLineComposeError as Compose,
    };
    // Style retirement may wrap the original Source failure; wrapping cannot lower its priority in
    // later cleanup merges.
    fn compose_source_io(error: &Compose) -> bool {
        match error {
            Compose::Source(
                Document::OriginalBackingUnavailable(_)
                | Document::EditStorageUnavailable(_)
                | Document::DisplayStorageUnavailable(Some(_)),
            ) => true,
            Compose::StyleResetCleanup { cause, .. } => compose_source_io(cause),
            _ => false, // Glyph-file release failure remains a cleanup error, rather than pretending to be document Source read failure.
        }
    }
    match error {
        Error::Fallback(cause) => cause.is_source_io(),
        Error::View(cause) => view_source_io(cause),
        Error::Compose(cause) => compose_source_io(cause),
        Error::Semantics(
            omni_markdown_core::markdown_document_buffer::MarkdownTableRequestError::Source(
                Document::OriginalBackingUnavailable(_)
                | Document::EditStorageUnavailable(_)
                | Document::DisplayStorageUnavailable(Some(_)),
            ),
        ) => true,
        Error::PreparationCleanup { cause, cleanup } => source_io(cause) || source_io(cleanup),
        _ => false, // Weak-scope expiry, stale-cell markers, and ordinary revocation cannot become primary Source I/O failures.
    }
}

/// Preparation retirement and actual painting share this View error classification; actual storage
/// errno alone outranks ordinary revocation.
/// Keep existing Source-primary order, without treating directory revocation or synchronization
/// corruption as disk failure.
pub(super) fn view_source_io(error: &ViewError) -> bool {
    use crate::native_surface::paged_input::NativeMarkdownPagedInputError as Input;
    use omni_markdown_core::MarkdownDocumentBufferError as Document;
    // Heading queries read the same original Source; anonymous slug/mask storage is not document Source
    // I/O.
    // Inspect its actual Cleanup wrapper for original Source causes, without changing priority because
    // the query was cancelled.
    fn heading_source_io(error: &omni_markdown_core::MarkdownHeadingAnchorError) -> bool {
        use omni_markdown_core::MarkdownHeadingAnchorError as Heading;
        use omni_markdown_core::markdown_document_buffer::MarkdownTableRequestError as Semantic;
        match error {
            Heading::Source(
                Document::OriginalBackingUnavailable(_)
                | Document::EditStorageUnavailable(_)
                | Document::DisplayStorageUnavailable(Some(_)),
            )
            | Heading::Semantic(Semantic::Source(
                Document::OriginalBackingUnavailable(_)
                | Document::EditStorageUnavailable(_)
                | Document::DisplayStorageUnavailable(Some(_)),
            )) => true,
            Heading::Cleanup { primary, cleanup } => {
                heading_source_io(primary) || heading_source_io(cleanup)
            }
            _ => false,
        }
    }
    if let ViewError::HeadingAnchor(error) = error {
        return heading_source_io(error);
    }
    matches!(
        error,
        ViewError::Document(
            Document::OriginalBackingUnavailable(_)
                | Document::EditStorageUnavailable(_)
                | Document::DisplayStorageUnavailable(Some(_)),
        ) | ViewError::Input(Input::Document(
            Document::OriginalBackingUnavailable(_)
                | Document::EditStorageUnavailable(_)
                | Document::DisplayStorageUnavailable(Some(_)),
        )) | ViewError::Input(Input::CellSource(
            omni_markdown_core::markdown_document_buffer::MarkdownTableRequestError::Source(
                Document::OriginalBackingUnavailable(_)
                    | Document::EditStorageUnavailable(_)
                    | Document::DisplayStorageUnavailable(Some(_)),
            ),
        ))
    )
}

/// Construct the actual font response once; both cache hits and complete worker trials revalidate the
/// original current Selection.
/// Font-choice facts expand no Source, coverage, or adjacent-semantic authority; return consumption to
/// the request original Core Composer.
fn selected_font_response(
    owner: &Owner,
    layout_source: &MarkdownLayoutTextSource,
    request: MarkdownLineStyleRequest,
    selection: super::semantics::Selection,
    base: &crate::native_surface::paged_line_fonts::NativeMarkdownLineFont,
    font: &crate::native_surface::paged_line_fonts::NativeMarkdownLineFont,
) -> Result<omni_markdown_core::MarkdownLineStyleResponse> {
    // This style selects the original semantic font only; testing the first grapheme does not certify
    // the entire request as free of
    // missing glyphs. The original Composer shapes actual ranges and the real painter rejects
    // missing-glyph candidates.
    selection.require(owner, layout_source, &request)?;
    let boundary = selection.style_boundary(owner, layout_source, &request)?;
    let inset =
        crate::native_surface::preview_paragraph::PreviewParagraph::from_styles(selection.styles())
            .map_or(0, |geometry| geometry.inset_26_6());
    let response = request
        .respond_registered(selection.coverage, font.registered(), selection.style)?
        // Seal the original semantic base font with this response; within the certified bounded shaping
        // segment Core
        // checks subsequent grapheme font choices so one fallback success cannot absorb later spaces.
        .with_preferred_registered_font(base.registered())?
        .with_paragraph_inset(inset)?;
    let response = if let Some(boundary) = boundary {
        // Attach the actual adjacent style certified by the same document, Source, and nonce; it does
        // not itself close a shaping item,
        // because Core must test the complete right grapheme and confirm a font change; color changes
        // do not create shaping boundaries.
        response.with_root_leaf_style_boundary(boundary)?
    } else {
        response
    };
    Ok(response)
}

struct FontTrial {
    target: FontTrialTarget,
    request: MarkdownLineStyleRequest,
    fallback: Option<NativeMarkdownFontFallback>,
    // The first shaping segment of a multisegment grapheme may still await original Bidi metadata;
    // invent no options before Ready.
    pending_probe: Option<MarkdownLineFontProbe>,
    selection: super::semantics::Selection,
    in_flight: bool,
    result: Option<NativeMarkdownFontTrialResult>,
    // Try the original semantic preferred font for every complete grapheme; previous fallback gains no
    // priority over following spaces/text.
    base: Rc<crate::native_surface::paged_line_fonts::NativeMarkdownLineFont>,
    // Keep one bounded complete-grapheme key and cache it only after actual Selected with original
    // glyph resources returned.
    cache_key: Option<super::font_choice::FontChoiceKey>,
}

/// Return worker results to their original actual Composer; cell-local ranges cannot answer canonical
/// body requests.
#[derive(Clone, Copy)]
enum FontTrialTarget {
    Body,
    Cell { row: usize, column: usize },
}

/// Describe this actual style delivery only: Core consumed a cached response, or original semantic/font
/// continuations still own Pending.
enum StyleStart {
    Cached { source_bytes: usize },
    Pending,
}

/// One active Body owns one actual worker, blocking on receive without polling or periodic wakes when
/// no packet exists.
pub(super) struct FontWorker {
    sender: Option<mpsc::SyncSender<(u64, NativeMarkdownFontTrial)>>,
    receiver: mpsc::Receiver<(u64, NativeMarkdownFontTrialResult)>,
    join: Option<JoinHandle<()>>,
    // Retain the final main-thread claim until join returns; dropping MainThreadBound on a background
    // thread synchronously dispatches to
    // main and must not deadlock with main-thread join. Notifications retain temporary copies of the
    // same weak identity only.
    _callback_owner: Arc<MainThreadBound<std::rc::Weak<Owner>>>,
}

impl FontWorker {
    fn new(owner: &Owner) -> Result<Self> {
        let (sender, requests) = mpsc::sync_channel::<(u64, NativeMarkdownFontTrial)>(1);
        let (results, receiver) = mpsc::channel();
        let weak = Arc::new(MainThreadBound::new(owner.weak.clone(), owner.mtm));
        let callback_owner = weak.clone();
        let join = std::thread::Builder::new()
            .name("omni-md-font-trial".into())
            .spawn(move || {
                while let Ok((serial, packet)) = requests.recv() {
                    let result = packet.run();
                    if results.send((serial, result)).is_err() {
                        break;
                    }
                    let weak = weak.clone();
                    // Completion notifications carry original weak identity and serial only; actual
                    // results stay in the original channel rather than letting
                    // callbacks retain Source, Native fonts, ShapeJobs, or another document copy.
                    crate::native_surface::enqueue_native_markdown_main_task(move || {
                        let mtm = MainThreadMarker::new().expect("字体完成只能在主队列交付");
                        let Some(owner) = weak.get(mtm).upgrade() else {
                            return;
                        };
                        if !owner
                            .worker
                            .borrow()
                            .as_ref()
                            .is_some_and(|worker| Arc::ptr_eq(&worker._callback_owner, &weak))
                        {
                            return; // The old worker actually joined/returned; late notifications cannot take equal-serial results from a successor worker.
                        }
                        if owner.preparing.get() {
                            // Nested Native delivery records the actual completion event only; the
                            // loaned Producer
                            // must return to its original slot first. One worker plus one queued packet
                            // permits at most two notifications.
                            let mut notices = owner.font_notices.borrow_mut();
                            if notices.len() == 2 {
                                owner.font_notice_failed.set(true);
                            } else {
                                notices.push_back(serial);
                            }
                            owner.deferred.set(true);
                            return;
                        }
                        if let Err(cause) = owner.finish_font_trial(serial)
                            && owner.active.get()
                        {
                            owner.fail(cause);
                        }
                    });
                }
            })
            .map_err(|_| Error::WorkerFailed)?;
        Ok(Self {
            sender: Some(sender),
            receiver,
            join: Some(join),
            _callback_owner: callback_owner,
        })
    }

    fn submit(&self, serial: u64, trial: NativeMarkdownFontTrial) -> Result<()> {
        self.sender
            .as_ref()
            .ok_or(Error::Closed)?
            .try_send((serial, trial))
            .map_err(|_| Error::WorkerFailed)
    }

    /// Consume one queue item only after its actual result notification arrives, without polling for
    /// thread completion.
    fn take_delivered(&self) -> Result<(u64, NativeMarkdownFontTrialResult)> {
        self.receiver.try_recv().map_err(|_| Error::WorkerFailed)
    }

    pub(super) fn close(mut self) -> Result<()> {
        drop(self.sender.take());
        let mut result = self
            .join
            .take()
            .map_or(Ok(()), |join| join.join().map_err(|_| Error::WorkerFailed));
        // After join the unique result sender ended; drain completed actual results only, without
        // polling or waiting for new work.
        // Even after join failure retire delivered results so receiver Drop cannot silently hide their
        // primary resource failure.
        for (_, delivered) in self.receiver.try_iter() {
            result =
                merge_preparation_cleanup(result, delivered.retire(false).map_err(Error::from));
        }
        result
    }
}
impl Drop for FontWorker {
    fn drop(&mut self) {
        drop(self.sender.take());
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

pub(super) struct Producer {
    source: MarkdownLayoutTextSource,
    domain: crate::native_surface::paged_input::NativeMarkdownPagedTargetDomain,
    composer: Option<MarkdownLineComposer>,
    cancellation: MarkdownLineComposeCancellation,
    generation: u64,
    fold_revision: u64,
    width: i64,
    /// Track one actual maximum right edge per Source/layout-width epoch, without retaining all-table
    /// column indices after offscreen retirement.
    maximum_content_width: f64,
    serial: u64,
    padding: f64,
    output: Option<Rc<RefCell<MarkdownComposedLines>>>,
    lines: usize,
    height: f64,
    complete: bool,
    step_bytes: usize,
    step_scalars: usize,
    step_metadata: usize,
    maximum_step_bytes: usize,
    maximum_step_scalars: usize,
    maximum_step_metadata: usize,
    trial_source_bytes: usize,
    trial: Option<FontTrial>,
    pending_style: Option<MarkdownLineStyleRequest>,
    pending_control: Option<omni_markdown_core::MarkdownLineInlineControl>,
    semantics: Option<super::semantics::RootLeaf>,
    grid: Option<Grid>,
    grids: VecDeque<Grid>,
    /// Keep one original row-directory restoration cursor, transferring it to visible rows on
    /// completion without historical Native indices.
    restoring_grid: Option<GridRestoration>,
    table_widths: Option<TableWidths>,
    cell_display_work: omni_markdown_core::markdown_document_buffer::SemanticWork,
    cell_frontier_source_bytes: usize,
    cell_shaped_input_bytes: usize,
    scan: Option<Scan>,
    pending_line: Option<PendingLine>,
    top: Option<TopPrefix>,
    top_lookup: Option<TopLookup>,
    target: Option<ActualTarget>,
    vertical: Option<VerticalTarget>,
    visual: Option<VisualWork>,
    visual_word: Option<omni_markdown_core::MarkdownComposedVisualWordQuery>,
    /// The original word slot owns one offscreen restored cell without an old Scene; cancellation first
    /// returns its same-request Source work.
    cell_word: Option<grid::word::CellWordQuery>,
    last_target_anchor: Option<MeasuredLineAnchor>,
    target_lookup_records: usize,
    target_metric_parts: usize,
    target_walk_lines: usize,
    target_index_records: usize,
    queued_lines: Option<QueuedLines>,
    pending_batch: Option<PendingBatch>,
    target_max_batch_lines: usize,
    target_checkpoint_attempts: usize,
    target_checkpoint_elapsed: std::time::Duration,
    target_batch_index_records: usize,
}

/// One bounded directory turn; only the owner decides whether to commit now or schedule continuation.
pub(super) enum VisualAdvance {
    Ready(super::word::VisualSelection),
    Pending,
    NeedPrefix,
    NeedsFoldExpansion(std::ops::Range<usize>),
}

/// A weak query bound to the original output; a later prefix replaces this ticket without another
/// reader or Source.
struct VisualWork {
    query: Option<omni_markdown_core::MarkdownComposedVisualQuery>,
    collapse: Option<MarkdownComposedLineLookup>,
}

/// Record a bounded interval of consecutive original LineReady results and final-line facts, without
/// all lines or glyphs.
struct QueuedLines {
    first: usize,
    count: usize,
    last: MarkdownComposedLineFacts,
}

/// At most 32 weak handles belong to one actual prefix Reader; return the small queue after ordered
/// measurement.
struct PendingBatch {
    output: Rc<RefCell<MarkdownComposedLines>>,
    first: usize,
    count: usize,
    cursor: Option<MarkdownComposedLine>,
    handles: VecDeque<MarkdownComposedLine>,
    collecting: bool,
}

/// Retain first-screen final ordinal/actual height and locate it through the latest unique Reader
/// index, without another snapshot.
/// Both scalars belong to the same Producer/Source/width lifetime and cannot survive edit/layout
/// supersession.
struct TopPrefix {
    last_ordinal: usize,
    height: f64,
}

struct TopLookup {
    request: NativeMarkdownPagedSceneRequest,
    cursor: MarkdownComposedLineLookup,
}

struct PendingLine {
    output: Rc<RefCell<MarkdownComposedLines>>,
    metrics: LineMetrics,
    target_batch: bool,
}

/// A LineEnd grants no unbounded part traversal; resume the same metric cursor each turn.
struct LineMetrics {
    handle: MarkdownComposedLine,
    part: usize,
    geometry: crate::native_surface::preview_paragraph::PreviewLineMetrics,
    ltr: bool,
    visible_end: usize,
    folded_end: Option<usize>,
    measure_alignment: bool,
    alignment: Option<NativeMarkdownLineAlignmentMeasure>,
}

struct Scan {
    request: NativeMarkdownPagedSceneRequest,
    cursor: Option<MarkdownComposedLine>,
    bottom: f64,
    first_top: f64,
    handles: VecDeque<ScannedLine>,
    ranges: VecDeque<std::ops::Range<usize>>,
    paragraphs: VecDeque<std::ops::Range<usize>>,
    folded_ends: VecDeque<Option<usize>>,
    simple_ltr: bool,
    metrics: Option<LineMetrics>,
    /// Actual table-row tops come from complete LineMetrics in this current window only, without
    /// historical height estimates.
    table_rows: VecDeque<(usize, f64)>,
}

/// Retain completed Native line heights for this bounded scan only, without text or historical line
/// positions.
/// At an original target anchor, reconstruct lower collected row positions forward from actual heights
/// rather than shifting a rounding difference.
struct ScannedLine {
    handle: NativeMarkdownMeasuredLine,
    height: f64,
}

impl Scan {
    /// The complete target request certified original top/bottom; lower windows cannot retain stale
    /// backward-subtracted prefix coordinates.
    /// Replay finite pure window metrics only, without Source/directory/font reads or another Reader.
    fn reanchor_table_rows(&mut self, anchor: &MeasuredLineAnchor) -> Result<()> {
        let mut top = anchor.bottom;
        let mut rows = self.table_rows.iter_mut();
        for measured in &self.handles {
            if measured.handle.facts().kind
                == omni_markdown_core::MarkdownComposedLineKind::TableRow
            {
                let (ordinal, row_top) = rows.next().ok_or(Error::Unsupported)?;
                if *ordinal != measured.handle.facts().ordinal {
                    return Err(Error::Unsupported); // Original lines and table entries must match one-to-one without another cell filling a missing entry.
                }
                *row_top = top;
            }
            top += measured.height;
            if !top.is_finite() || top >= (1_u64 << 46) as f64 {
                return Err(ViewError::InvalidGeometry.into());
            }
        }
        if rows.next().is_some() {
            return Err(Error::Unsupported); // Retain no table position lacking its original weak line and actual height.
        }
        Ok(())
    }
}

/// The actual original Reader query shares restored-cell lifetime, without another document, parser,
/// Input, or whole-table position cache.
struct GridRestoration {
    output: Rc<RefCell<MarkdownComposedLines>>,
    lookup: Option<MarkdownComposedLineLookup>,
    line: Option<MarkdownComposedLine>,
    top: f64,
    height: f64,
    grid: Option<Grid>,
}

impl Producer {
    pub(super) fn new(
        source: MarkdownLayoutTextSource,
        request: &NativeMarkdownPagedSceneRequest,
        catalog: MarkdownLineFontCatalog,
        padding: f64,
        serial: u64,
        mode: omni_markdown_core::MarkdownPresentationMode,
        folds: &[std::ops::Range<usize>],
    ) -> Result<Self> {
        request
            .target
            .require_source(&source)
            .map_err(ViewError::Input)?;
        let width = width(request, padding)?;
        let cancellation = MarkdownLineComposeCancellation::new();
        // Inline preview admits actual table-flow requests; Source keeps the literal frontier. Both own the same canonical Source.
        let mut composer = if mode == omni_markdown_core::MarkdownPresentationMode::InlinePreview {
            MarkdownLineComposer::new_with_table_flow(
                source.clone(),
                &source.authority(),
                catalog,
                width,
                cancellation.clone(),
            )?
        } else {
            MarkdownLineComposer::new_with_frontier(
                source.clone(),
                &source.authority(),
                catalog,
                width,
                cancellation.clone(),
            )?
        };
        if !folds.is_empty() {
            composer.install_folded_ranges(folds)?;
        } // Install bounded physical-line proofs before the original Source Composer first step, without another document or glyph graph.
        let semantics = (mode == omni_markdown_core::MarkdownPresentationMode::InlinePreview)
            .then(|| super::semantics::RootLeaf::new(&source, serial));
        Ok(Self {
            generation: source.generation(),
            fold_revision: request.input.fold_revision,
            domain: request.target.clone(),
            source,
            composer: Some(composer),
            cancellation,
            width,
            maximum_content_width: request.virtual_viewport.layout_width,
            serial,
            padding,
            output: None,
            lines: 0,
            height: 0.0,
            complete: false,
            step_bytes: 0,
            step_scalars: 0,
            step_metadata: 0,
            maximum_step_bytes: 0,
            maximum_step_scalars: 0,
            maximum_step_metadata: 0,
            trial_source_bytes: 0,
            trial: None,
            pending_style: None,
            pending_control: None,
            semantics,
            grid: None,
            grids: VecDeque::new(),
            restoring_grid: None,
            table_widths: None,
            cell_display_work: Default::default(),
            cell_frontier_source_bytes: 0,
            cell_shaped_input_bytes: 0,
            scan: None,
            pending_line: None,
            top: None,
            top_lookup: None,
            target: None,
            vertical: None,
            visual: None,
            visual_word: None,
            cell_word: None,
            last_target_anchor: None,
            target_lookup_records: 0,
            target_metric_parts: 0,
            target_walk_lines: 0,
            target_index_records: 0,
            queued_lines: None,
            pending_batch: None,
            target_max_batch_lines: 0,
            target_checkpoint_attempts: 0,
            target_checkpoint_elapsed: std::time::Duration::ZERO,
            target_batch_index_records: 0,
        })
    }
    /// Check original content/domain/fold/width identity; same-Source selection changes have their explicit reprojection path below.
    pub(super) fn accepts(&self, request: &NativeMarkdownPagedSceneRequest) -> bool {
        self.domain.same(&request.target)
            && self.generation == request.input.generation
            && self.fold_revision == request.input.fold_revision
            && self.source.utf16_len() == request.input.utf16_len
            && width(request, self.padding).is_ok_and(|width| width == self.width)
    }

    /// Selection changes reproject same-Source styles without reusing old hidden glyphs or rebuilding
    /// original language indices.
    /// False means the original font worker has not returned; its completion event owns wakeup without
    /// main-thread polling.
    pub(super) fn refresh_semantic_selection(
        &mut self,
        owner: &Owner,
        request: &NativeMarkdownPagedSceneRequest,
    ) -> Result<bool> {
        if !self
            .semantics
            .as_ref()
            .is_some_and(|semantic| semantic.selection_changed(request.input.selection))
        {
            return Ok(true); // Source mode or unchanged actual selection preserves original prefix/Reader and fonts.
        }
        let ticket =
            PreparationTicket::new(request, &self.source, self.serial, self.width, self.padding);
        ticket.require_pure(owner, &self.source)?;
        if let Some(mut trial) = self.trial.take() {
            if let Some(fallback) = trial.fallback.take() {
                fallback.close(); // Cancel the old font trial first; old selection cannot deliver its packet into new semantics.
            }
            if trial.in_flight {
                self.trial = Some(trial);
                return Ok(false); // Preserve the unique result slot; reproject only after receiving and retiring the original worker packet.
            }
            if let Some(result) = trial.result.take() {
                result.retire(false)?;
            } // Actual Source/font cleanup failure retains its original error path and cannot be overwritten by a new empty graph.
        }
        // Native accumulated Y agrees with the same Store latest LineEnd only after every published
        // line completes original measurement.
        // Incomplete batches use full style reset so sealed lines not yet included in height are not
        // lost.
        let retain_composer = self.output.is_some()
            && self.pending_line.is_none()
            && self.pending_batch.is_none()
            && self.queued_lines.is_none()
            && self.pending_style.is_none()
            && self.pending_control.is_none()
            && self.grid.is_none()
            && self.grids.is_empty()
            && self.restoring_grid.is_none()
            && self.cell_word.is_none()
            && self
                .semantics
                .as_mut()
                .expect("选择显隐只属于当前 Preview 语义")
                .reproject_selection_preserving_styles(
                    owner,
                    &self.source,
                    request.input.selection,
                    self.composer.as_ref().ok_or(Error::Closed)?,
                    &ticket,
                )?;
        #[cfg(feature = "native-markdown-runtime-probe")]
        eprintln!(
            "native_selection_layout retained={retain_composer} prepared_lines={} selection_serial={} generation={} output_ready={} line_pending={} batch_pending={} queued_lines={} style_pending={} control_pending={} grid_active={} visible_grids={} restoring_grid={} cell_word={}",
            self.lines,
            request.input.selection_serial,
            request.input.generation,
            self.output.is_some(),
            self.pending_line.is_some(),
            self.pending_batch.is_some(),
            self.queued_lines.is_some(),
            self.pending_style.is_some(),
            self.pending_control.is_some(),
            self.grid.is_some(),
            self.grids.len(),
            self.restoring_grid.is_some(),
            self.cell_word.is_some(),
        ); // At selection supersession report actual owner slots only, without clearing them to force retention or logging text.
        self.pending_style = None;
        self.pending_control = None;
        self.grid = None;
        self.grids.clear();
        self.restoring_grid = None;
        self.cell_word = None; // Selection superseded; retire the same old Request and its bounded cell-reading graph together.
        self.table_widths = None;
        self.scan = None;
        self.pending_line = None;
        self.pending_batch = None;
        self.queued_lines = None;
        self.top_lookup = None;
        self.top = None;
        self.target = None;
        self.vertical = None;
        self.visual = None;
        self.visual_word = None;
        self.last_target_anchor = None;
        drop(self.output.take()); // Body cleared the old Scene first; equal glyphs cannot let old output identity certify new Input.
        ticket.require_pure(owner, &self.source)?;
        if retain_composer {
            // The same stack retired the original SourceRequest publication; glyphs, original font
            // registration, and
            // resource scope keep their original owner. Duplicate the same writer committed prefix
            // while old weak Line/Part
            // identities remain revoked; successor Scene/CPU use complete new Input and output identity
            // without old-frame reuse.
            self.output = Some(self.checkpoint_prefix(false)?);
            owner.input.clear_style_navigation_geometry();
            ticket.require_pure(owner, &self.source)?;
            return Ok(true);
        }
        self.composer
            .as_mut()
            .ok_or(Error::Closed)?
            .reset_styles(owner.catalog.clone())?;
        self.semantics
            .as_mut()
            .expect("选择失效只属于实际InlinePreview语义")
            .reset_selection(request.input.selection);
        self.lines = 0;
        self.height = 0.0;
        self.maximum_content_width = request.virtual_viewport.layout_width;
        self.complete = false;
        // Same-Source selection visibility preserves committed affinity intent while retiring old
        // target columns/Bidi parts.
        // New Source/width still performs full navigation-geometry clearing; this narrow retention
        // cannot cross supersession.
        owner.input.clear_style_navigation_geometry();
        ticket.require_pure(owner, &self.source)?;
        Ok(true)
    }
    /// Actual cell edits revoke old reads; retire preparation only after the same Input publishes
    /// successor content, preserving actual EIO failure.
    pub(super) fn replaced_cell_source(
        &self,
        input: &crate::native_surface::paged_input::NativeMarkdownPagedInput,
        request: &NativeMarkdownPagedSceneRequest,
    ) -> Result<bool> {
        if !input.is_cell() {
            return Ok(false); // Keep original frozen body-Source reads and budgets unchanged.
        }
        match self.source.byte_offset_for_utf16(0) {
            Ok(_) => Ok(false),
            Err(omni_markdown_core::MarkdownDocumentBufferError::PreparedTextInputStale)
                if self.source.cell_draft_authority().is_some()
                    && input
                        .state()
                        .is_ok_and(|actual| actual.generation != request.input.generation) =>
            {
                Ok(true) // Construction verified the original cell domain; this is not silent recovery for arbitrary Source errors or cancelled sessions.
            }
            Err(cause) => Err(ViewError::Document(cause).into()),
        }
    }
    pub(super) fn owns_target_source(&self, key: &NativeMarkdownPagedSceneKey) -> bool {
        self.source.authority() == key.source && self.accepts(&key.request) && self.output.is_some()
    }
    pub(super) fn target_source_authority(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        serial: u64,
    ) -> Result<omni_markdown_core::MarkdownLayoutTextAuthority> {
        self.target_source_health()?;
        if self.serial != serial || !self.accepts(request) {
            return Err(ViewError::Revoked.into());
        }
        Ok(self.source.authority()) // Borrow existing frozen Source identity without capture or another prefix Reader.
    }
    pub(super) fn target_source_health(&self) -> Result<()> {
        self.source
            .byte_offset_for_utf16(0)
            .map_err(ViewError::Document)?;
        Ok(())
    }
    /// Before Drop, word commit obtains a weak release observer from the same Composer without output
    /// or another Source reader.
    pub(super) fn word_retirement_resources(
        &self,
    ) -> Result<omni_markdown_core::MarkdownLineComposerWorkerResourceObserver> {
        Ok(self
            .composer
            .as_ref()
            .ok_or(Error::Closed)?
            .worker_resource_observer())
    }
    /// Word deletion reuses original Composer payload/Source; begin certifies a caret without lines,
    /// Readers, or font trials.
    pub(super) fn begin_word_delete(&mut self, forward: bool, head: usize) -> Result<()> {
        self.target_source_health()?;
        let direction = if forward {
            omni_markdown_core::MarkdownWordDeleteDirection::Forward
        } else {
            omni_markdown_core::MarkdownWordDeleteDirection::Backward
        };
        self.composer
            .as_mut()
            .ok_or(Error::Closed)?
            .begin_word_delete(direction, head)?;
        self.target_source_health()
    }

    /// Ordinary deletion borrows actual adjacent graphemes from the same Composer without fonts, line
    /// geometry, or another document reader.
    pub(super) fn begin_grapheme_delete(&mut self, forward: bool, head: usize) -> Result<()> {
        self.target_source_health()?;
        let direction = if forward {
            omni_markdown_core::MarkdownWordDeleteDirection::Forward
        } else {
            omni_markdown_core::MarkdownWordDeleteDirection::Backward
        };
        self.composer
            .as_mut()
            .ok_or(Error::Closed)?
            .begin_grapheme_delete(direction, head)?;
        self.target_source_health()
    }

    /// Advance one shared 4096-byte quantum per original main-queue turn; huge graphemes span steps
    /// without scalar-based truncation.
    pub(super) fn step_word_delete(
        &mut self,
        owner: &Owner,
        request: &NativeMarkdownPagedSceneRequest,
    ) -> Result<omni_markdown_core::MarkdownWordDeleteAnswer> {
        let ticket =
            PreparationTicket::new(request, &self.source, self.serial, self.width, self.padding);
        ticket.require_pure(owner, &self.source)?;
        let step = self
            .composer
            .as_mut()
            .ok_or(Error::Closed)?
            .step_word_delete(MarkdownLineComposeBudget {
                bytes: 4096,
                scalars: 256,
                records: 256,
                metadata_units: 256,
            })?;
        // Account actual quantum reads in existing Composer progress, without another set of acceptance
        // counters.
        self.step_bytes = self
            .step_bytes
            .checked_add(step.bytes)
            .ok_or(Error::CounterExhausted)?;
        self.step_scalars = self
            .step_scalars
            .checked_add(step.scalars)
            .ok_or(Error::CounterExhausted)?;
        self.step_metadata = self
            .step_metadata
            .checked_add(step.metadata_units)
            .ok_or(Error::CounterExhausted)?;
        self.maximum_step_bytes = self.maximum_step_bytes.max(step.bytes);
        self.maximum_step_scalars = self.maximum_step_scalars.max(step.scalars);
        self.maximum_step_metadata = self.maximum_step_metadata.max(step.metadata_units);
        ticket.require(owner, &self.source)?; // An old Ready range loses commit authority after actual Native geometry revocation.
        Ok(step.answer)
    }

    /// Cancel the original payload word continuation only; this Producer still owns existing layout,
    /// Source, and Reader.
    pub(super) fn cancel_word_delete(&mut self) {
        if let Some(composer) = self.composer.as_mut() {
            composer.cancel_word_delete();
        }
    }

    /// Word selection borrows original Composer/graphemes and Source, without another read capture or a
    /// current LineReady requirement.
    pub(super) fn begin_word_group(
        &mut self,
        action: omni_markdown_core::MarkdownWordGroupAction,
        head: usize,
        cell: Option<crate::native_surface::paged_text_view::NativeMarkdownPagedCellWordTarget>,
    ) -> Result<()> {
        self.target_source_health()?;
        if let Some(original) = self.cell_word.as_mut() {
            if !original.cancelled || original.restart.is_some() {
                return Err(ViewError::Busy.into());
            }
            original.restart = Some((action, head, cell));
            return Ok(()); // Record the accepted next head only; start no successor read until actual original CellInline return.
        }
        if let Some(cell) = cell {
            self.cell_word = Some(grid::word::CellWordQuery::new(cell, head, action));
            return Ok(());
        }
        self.composer
            .as_mut()
            .ok_or(Error::Closed)?
            .begin_word_group(action, head)?;
        self.target_source_health()
    }

    /// The original Composer selects full-Root or per-visible-segment certificates for visual movement;
    /// logical selection groups gain no extra authority.
    pub(super) fn begin_visual_word_group(&mut self, forward: bool, head: usize) -> Result<()> {
        self.target_source_health()?;
        self.composer
            .as_mut()
            .ok_or(Error::Closed)?
            .begin_visual_word_group(
                if forward {
                    omni_markdown_core::MarkdownWordDeleteDirection::Forward
                } else {
                    omni_markdown_core::MarkdownWordDeleteDirection::Backward
                },
                head,
            )?;
        self.target_source_health()
    }

    /// Visual word movement may use logical groups only for original full-Source UniformZero Bidi;
    /// preparation and grapheme work receive no duplicate quantum.
    pub(super) fn prepare_word_move(
        &mut self,
        owner: &Owner,
        request: &NativeMarkdownPagedSceneRequest,
    ) -> Result<Option<omni_markdown_core::MarkdownWordMoveScope>> {
        let ticket =
            PreparationTicket::new(request, &self.source, self.serial, self.width, self.padding);
        ticket.require_pure(owner, &self.source)?;
        let step = self
            .composer
            .as_mut()
            .ok_or(Error::Closed)?
            .prepare_word_move(MarkdownLineComposeBudget {
                bytes: 4096,
                scalars: 256,
                records: 256,
                metadata_units: 256,
            })?;
        self.step_bytes = self
            .step_bytes
            .checked_add(step.bytes)
            .ok_or(Error::CounterExhausted)?;
        self.step_scalars = self
            .step_scalars
            .checked_add(step.scalars)
            .ok_or(Error::CounterExhausted)?;
        self.step_metadata = self
            .step_metadata
            .checked_add(step.metadata_units)
            .ok_or(Error::CounterExhausted)?;
        self.maximum_step_bytes = self.maximum_step_bytes.max(step.bytes);
        self.maximum_step_scalars = self.maximum_step_scalars.max(step.scalars);
        self.maximum_step_metadata = self.maximum_step_metadata.max(step.metadata_units);
        ticket.require(owner, &self.source)?; // Completion cannot replace post-call validation of original Native geometry/Input authority.
        Ok(step.scope)
    }

    /// One main-queue turn consumes one original 4 KiB/256 quantum, recorded in the same Producer
    /// actual work totals.
    pub(super) fn step_word_group(
        &mut self,
        owner: &Owner,
        request: &NativeMarkdownPagedSceneRequest,
    ) -> Result<omni_markdown_core::MarkdownWordGroupAnswer> {
        if self.cell_word.is_some() {
            return self.step_cell_word_group(owner, request);
        }
        let ticket =
            PreparationTicket::new(request, &self.source, self.serial, self.width, self.padding);
        ticket.require_pure(owner, &self.source)?;
        let step = self
            .composer
            .as_mut()
            .ok_or(Error::Closed)?
            .step_word_group(MarkdownLineComposeBudget {
                bytes: 4096,
                scalars: 256,
                records: 256,
                metadata_units: 256,
            })?;
        self.step_bytes = self
            .step_bytes
            .checked_add(step.bytes)
            .ok_or(Error::CounterExhausted)?;
        self.step_scalars = self
            .step_scalars
            .checked_add(step.scalars)
            .ok_or(Error::CounterExhausted)?;
        self.step_metadata = self
            .step_metadata
            .checked_add(step.metadata_units)
            .ok_or(Error::CounterExhausted)?;
        self.maximum_step_bytes = self.maximum_step_bytes.max(step.bytes);
        self.maximum_step_scalars = self.maximum_step_scalars.max(step.scalars);
        self.maximum_step_metadata = self.maximum_step_metadata.max(step.metadata_units);
        ticket.require(owner, &self.source)?; // Even an actual range lacks original-permit commit authority after geometry or Input revocation.
        Ok(step.answer)
    }

    /// Remove this slot word continuation only; the unique Producer still owns and actually closes
    /// original fonts, layout, and Source.
    pub(super) fn cancel_word_group(&mut self) {
        if let Some(mut query) = self.cell_word.take() {
            for grid in self.grid.iter_mut().chain(self.grids.iter_mut()).chain(
                self.restoring_grid
                    .iter_mut()
                    .filter_map(|restoring| restoring.grid.as_mut()),
            ) {
                grid.cancel_word(&query);
            }
            query.restart = None;
            if !query.cancel() {
                self.cell_word = Some(query); // Cancellation intent does not mean actual return of the SourceRequest cell task.
            }
        }
        if let Some(composer) = self.composer.as_mut() {
            composer.cancel_word_group();
        }
    }
    pub(super) fn height(&self) -> f64 {
        (self.height + self.padding * 2.0).max(
            self.grid
                .as_ref()
                .map_or(0.0, |grid| grid.top + grid.height() + self.padding),
        )
    }

    /// Actual EOF alone ends prefix production; in-flight table cells contribute their current actual
    /// measured heights.
    pub(super) fn needs_document_prefix(&self, bottom: f64) -> bool {
        !self.complete && self.height() < bottom
    }

    /// Observe actual existing task phases and work facts without advancing Source, creating Readers,
    /// or claiming Ready.
    pub(super) fn progress(&self) -> Result<NativeMarkdownPagedBodyProgress> {
        let work = self
            .composer
            .as_ref()
            .ok_or(Error::Closed)?
            .frontier_work()?;
        let mut semantic_work = self
            .semantics
            .as_ref()
            .map_or_else(Default::default, super::semantics::RootLeaf::current_work);
        add_semantic_work(&mut semantic_work, self.cell_display_work)?;
        Ok(NativeMarkdownPagedBodyProgress {
            phase: if self.pending_batch.is_some() {
                "target-batch-metrics"
            } else if self.pending_line.is_some() {
                "line-metrics"
            } else if self.trial.as_ref().is_some_and(|trial| trial.in_flight) {
                "font-worker"
            } else if self.trial.is_some() {
                "font-choice"
            } else if self.pending_style.is_some() {
                "root-leaf-semantics"
            } else if self.scan.is_some() {
                "window-scan"
            } else if self.top_lookup.is_some() {
                "top-line-lookup"
            } else if self.queued_lines.is_some() {
                "target-compose-batch"
            } else {
                "composer"
            },
            scheduled_serial: None,
            producer_serial: 0,
            revoked_preparations: 0,
            preparing: false,
            metadata_part_cursor: self
                .pending_line
                .as_ref()
                .map(|line| line.metrics.part)
                .or_else(|| {
                    self.scan
                        .as_ref()
                        .and_then(|scan| scan.metrics.as_ref().map(|metrics| metrics.part))
                }),
            scan_cursor_line: self
                .scan
                .as_ref()
                .and_then(|scan| scan.cursor.as_ref().map(|line| line.facts().ordinal)),
            scan_collected_lines: self.scan.as_ref().map_or(0, |scan| scan.handles.len()),
            font_trial_in_flight: self.has_in_flight(),
            prepared_lines: self.lines,
            prepared_height: self.height(),
            frontier_source_bytes: work
                .source_bytes
                .checked_add(self.cell_frontier_source_bytes)
                .ok_or(Error::CounterExhausted)?,
            shaped_input_bytes: work
                .input_bytes
                .checked_add(self.cell_shaped_input_bytes)
                .ok_or(Error::CounterExhausted)?,
            composer_step_bytes: self.step_bytes,
            composer_step_scalars: self.step_scalars,
            composer_step_metadata_units: self.step_metadata,
            maximum_step_bytes: self.maximum_step_bytes,
            maximum_step_scalars: self.maximum_step_scalars,
            maximum_step_metadata_units: self.maximum_step_metadata,
            trial_source_bytes: self.trial_source_bytes,
            semantic_work,
            source_total_bytes: self.source.byte_len(),
            complete: self.complete,
            target_lookup_records: self.target_lookup_records,
            target_metric_parts: self.target_metric_parts,
            target_walk_lines: self.target_walk_lines,
            target_index_records: self.target_index_records,
            target_unmeasured_lines: self.queued_lines.as_ref().map_or(0, |batch| batch.count)
                + self.pending_batch.as_ref().map_or_else(
                    || {
                        usize::from(
                            self.pending_line
                                .as_ref()
                                .is_some_and(|line| line.target_batch),
                        )
                    },
                    |batch| batch.first + batch.count - self.lines,
                ),
            target_unmeasured_last_source: self
                .queued_lines
                .as_ref()
                .map(|batch| batch.last.source_utf16.clone()),
            target_max_batch_lines: self.target_max_batch_lines,
            target_checkpoint_attempts: self.target_checkpoint_attempts,
            target_checkpoint_millis: self.target_checkpoint_elapsed.as_secs_f64() * 1000.0,
            target_batch_index_records: self.target_batch_index_records,
        })
    }
    pub(super) fn has_in_flight(&self) -> bool {
        self.trial.as_ref().is_some_and(|trial| trial.in_flight)
    }

    /// Active-cell navigation briefly borrows the unique Request after original display Source/font
    /// work returns;
    /// an in-flight navigation resumes its own quantum without another viewport CellInline taking
    /// ownership.
    pub(super) fn navigate_cell(
        &mut self,
        owner: &Owner,
        row: &omni_markdown_core::VerifiedMarkdownLayoutTableRow,
        column: usize,
        action: crate::native_surface::paged_text_view::NativeMarkdownPagedCellAction,
    ) -> Result<super::semantics::CellNavigationStep> {
        self.target_source_health()?;
        let semantics = self.semantics.as_mut().ok_or(Error::Unsupported)?;
        if !semantics.request_started()
            || !semantics.navigating_cell()
                && (self.trial.is_some()
                    || self.pending_style.is_some()
                    || self.pending_control.is_some()
                    || self.grid.as_ref().is_some_and(Grid::has_preparation)
                    || self.grids.iter().any(Grid::has_preparation)
                    || self.restoring_grid.as_ref().is_some_and(|restoring| {
                        restoring.grid.as_ref().is_some_and(Grid::has_preparation)
                    }))
        {
            return Ok(super::semantics::CellNavigationStep::Occupied);
        }
        let document = owner
            .document
            .borrow()
            .as_ref()
            .expect("格导航只归原 canonical Body")
            .clone();
        let step =
            semantics.navigate_cell(&document.borrow(), &self.source, row, column, action)?;
        self.target_source_health()?;
        Ok(step)
    }

    /// Esc returns actual Source work for this original navigation only; an unstarted intent ends
    /// without recreating the whole Request budget.
    pub(super) fn drain_cell_navigation(&mut self, owner: &Owner) -> Result<bool> {
        self.target_source_health()?;
        let document = owner
            .document
            .borrow()
            .as_ref()
            .expect("格取消仍归原 canonical Body")
            .clone();
        let result = self
            .semantics
            .as_mut()
            .expect("活动格只来自原预览来源请求")
            .drain_cell_navigation(&document.borrow(), &self.source);
        self.target_source_health()?; // The same original Source failure precedes ordinary cancellation or authority revocation.
        result
    }

    pub(super) fn activate_cell(
        &mut self,
        owner: &Owner,
        position: usize,
        column: usize,
        next_row: bool,
    ) -> Result<super::semantics::CellNavigationStep> {
        self.target_source_health()?;
        let semantics = self.semantics.as_mut().ok_or(Error::Unsupported)?;
        if !semantics.request_started()
            || !semantics.navigating_cell()
                && (self.trial.is_some()
                    || self.pending_style.is_some()
                    || self.pending_control.is_some()
                    || self.grid.as_ref().is_some_and(Grid::has_preparation)
                    || self.grids.iter().any(Grid::has_preparation))
        {
            return Ok(super::semantics::CellNavigationStep::Occupied);
        }
        let document = owner
            .document
            .borrow()
            .as_ref()
            .expect("激活沿原正文")
            .clone();
        let step = semantics.activate_cell(
            &document.borrow(),
            &self.source,
            position,
            column,
            next_row,
        )?;
        self.target_source_health()?;
        Ok(step)
    }

    #[cfg(all(
        feature = "native-paged-body-probe",
        feature = "native-markdown-runtime-probe"
    ))]
    pub(super) fn root_leaf_publication(
        &self,
    ) -> Option<std::rc::Weak<omni_markdown_core::VerifiedMarkdownViewportSources>> {
        self.semantics
            .as_ref()
            .and_then(super::semantics::RootLeaf::publication)
    }

    #[cfg(all(
        feature = "native-paged-body-probe",
        feature = "native-markdown-runtime-probe"
    ))]
    pub(super) fn root_leaf_parts(
        &self,
        owner: &Owner,
    ) -> Result<Vec<super::semantics::RootLeafPart>> {
        self.target_source_health()?;
        let output = self.output.as_ref().ok_or(Error::Busy)?;
        let mut output = output.borrow_mut();
        let fonts = owner.fonts.borrow();
        let typography = owner.typography.borrow();
        let typography = typography.as_ref().ok_or(Error::Unsupported)?;
        let mut line = output.last_line()?;
        let mut result = Vec::new();
        let mut count = 0;
        // The original Reader batch holds at most 256 glyphs; reuse fixed stack storage without another
        // layout or Source-reading graph.
        let mut glyphs = [omni_markdown_core::MarkdownShapedGlyph {
            glyph_id: 0,
            source_utf16_cluster: 0,
            flags: 0,
            x_advance: 0,
            y_advance: 0,
            x_offset: 0,
            y_offset: 0,
        }; 256];
        while let Some(actual) = line {
            if count == 32 {
                return Err(Error::Unsupported);
            }
            count += 1;
            for ordinal in 0..actual.facts().parts {
                if result.len() == 256 {
                    return Err(Error::Unsupported);
                }
                let part = output.part(&actual, ordinal)?.ok_or(Error::Unsupported)?;
                if part.facts().glyphs == 0 {
                    continue;
                } // Actual newlines/empty lines do not create invented glyph-font assertions.
                let authority = part.font_registration().authority()?;
                let index = fonts
                    .iter()
                    .position(|font| font.native().core_font().permits(&authority))
                    .ok_or(Error::Unsupported)?;
                fonts[index].require(part.font_registration())?;
                // Read glyphs sealed by the original LineEnd only. Large parts retain metadata
                // observation,
                // but expose no complete glyph result; neither enlarge storage nor reshape another
                // result as original output.
                let observed_glyphs = if part.facts().glyphs <= glyphs.len() {
                    let used = part.facts().glyphs;
                    output.read_batch(&part, 0..used, &mut glyphs[..used])?;
                    Some(glyphs[..used].to_vec())
                } else {
                    None
                };
                result.push(super::semantics::RootLeafPart {
                    source: part.facts().source_utf16.clone(),
                    advance_26_6: part.facts().x_advance_26_6,
                    script: part.facts().script,
                    direction: part.facts().direction,
                    glyphs: observed_glyphs,
                    presentation: part.facts().presentation,
                    points: part.facts().points,
                    native_points: fonts[index].native().point_size(),
                    role: typography.role_at(index, part.facts().presentation),
                    traits: fonts[index].traits(),
                    metrics: fonts[index].metrics()?,
                    font_owner: Rc::downgrade(&fonts[index]),
                });
            }
            line = output.previous_line(&actual)?;
        }
        self.target_source_health()?;
        Ok(result)
    }

    /// Link queries consume this Producer original Source and actual Ready without new document capture
    /// or parser advancement on the mouse stack.
    pub(super) fn link_at(
        &self,
        owner: &Owner,
        key: &NativeMarkdownPagedSceneKey,
        index: usize,
    ) -> std::result::Result<Option<omni_markdown_core::MarkdownSourceLink>, ViewError> {
        if key.source != self.source.authority() {
            return Err(ViewError::Revoked);
        }
        for grid in self.grids.iter().chain(self.grid.iter()).chain(
            self.restoring_grid
                .iter()
                .filter_map(|restoring| restoring.grid.as_ref()),
        ) {
            if let Some(link) = grid.link_at(owner, index).map_err(ViewError::LinkSource)? {
                return Ok(link); // Only actual cell coverage answers; otherwise use the original ordinary-body recovery path.
            }
        }
        let semantics = self.semantics.as_ref().ok_or(ViewError::Pending)?;
        semantics.link_at(owner, &self.source, index)
    }

    /// Link recovery borrows original Composer/font ownership, waiting for in-flight notifications and
    /// retiring delivered trials through their original path.
    pub(super) fn advance_link(
        &mut self,
        owner: &Owner,
        key: &NativeMarkdownPagedSceneKey,
        index: usize,
    ) -> Result<Option<Option<omni_markdown_core::MarkdownSourceLink>>> {
        let ticket = PreparationTicket::new(
            &key.request,
            &self.source,
            self.serial,
            self.width,
            self.padding,
        );
        ticket.require_pure(owner, &self.source)?;
        if key.source != self.source.authority() {
            return Err(ViewError::Revoked.into());
        }
        if self.trial.is_some() {
            self.advance_trial(owner, &ticket)?;
            return Ok(None); // Do not advance semantics concurrently, cancel the actual FontTrial, or synchronously join a background Source user.
        }
        for grid in self.grids.iter().chain(self.grid.iter()).chain(
            self.restoring_grid
                .iter()
                .filter_map(|restoring| restoring.grid.as_ref()),
        ) {
            // mouseUp retains original Scene/Source/gesture authority; cell targets consume the actual
            // display original parser publication only.
            // Equal ranges, visible LINK bits, or target strings cannot replace this actual source
            // query.
            let result = grid.link_at(owner, index);
            self.source
                .byte_offset_for_utf16(0)
                .map_err(ViewError::Document)?;
            if let Some(link) = result.map_err(ViewError::LinkSource)? {
                ticket.require_pure(owner, &self.source)?;
                return Ok(Some(link));
            }
        }
        let semantic = self.semantics.as_mut().ok_or(Error::Unsupported)?;
        let result = semantic.advance_link(owner, &self.source, index, &ticket);
        self.source
            .byte_offset_for_utf16(0)
            .map_err(ViewError::Document)?;
        result
    }

    /// Explicit retirement ends original fallback authority before actual Producer Drop, preserving
    /// result errors beyond destructor boundaries.
    pub(super) fn retire(mut self: Box<Self>) -> Result<()> {
        let backing = self.source.original_backing_observer();
        self.cancellation.cancel();
        let result = if let Some(trial) = self.trial.as_mut() {
            if let Some(fallback) = trial.fallback.take() {
                fallback.close(); // Set original cancellation first so precisely stale-cell revocation may qualify as ordinary retirement.
            }
            trial
                .result
                .take()
                .map_or(Ok(()), |result| result.retire(false).map_err(Error::from))
        } else {
            Ok(()) // Without a constructed trial there is no result to consume and no fabricated zero receipt.
        };
        drop(self); // Check final destructor errors after actual retirement of original Source, Readers, semantics, Composer, and remaining trials.
        if let Some(errno) = backing.as_ref().and_then(|backing| backing.first_errno()) {
            if matches!(
                &result,
                Err(Error::Fallback(crate::native_surface::paged_font_fallback::NativeMarkdownFontFallbackError::Source(
                    omni_markdown_core::MarkdownDocumentBufferError::OriginalBackingUnavailable(original),
                ))) if *original == errno
            ) {
                return result; // Reading the same errno from the same observer is not another cleanup failure and must not fabricate a compound error.
            }
            let source =
                crate::native_surface::paged_font_fallback::NativeMarkdownFontFallbackError::Source(
                    omni_markdown_core::MarkdownDocumentBufferError::OriginalBackingUnavailable(
                        errno,
                    ),
                );
            return merge_preparation_cleanup(Err(source.into()), result);
        }
        result
    }

    pub(super) fn receipts(&self) -> Result<NativeMarkdownPagedBodyResourceReceipts> {
        let composer = self.composer.as_ref().ok_or(Error::Closed)?;
        // A new prefix may already own a Reader during bounded measurement; observe that same instance
        // without publishing/duplicating another for receipts.
        let reader = self
            .output
            .as_ref()
            .or_else(|| self.pending_line.as_ref().map(|line| &line.output))
            .or_else(|| self.pending_batch.as_ref().map(|batch| &batch.output))
            .map(|output| output.borrow().resource_observer());
        Ok(NativeMarkdownPagedBodyResourceReceipts {
            source: self.source.resource_observer(),
            reader,
            writer: composer.resource_observer()?,
            worker: composer.worker_resource_observer(),
            fonts: Vec::new(),
            cells: self.cell_resource_receipts()?,
        })
    }

    /// Aggregate currently owned bounded cell observers only, without Readers, old-cell retention, or
    /// invented historical release facts.
    pub(super) fn cell_resource_receipts(
        &self,
    ) -> Result<Vec<super::NativeMarkdownPagedCellResourceReceipts>> {
        let mut receipts = Vec::new();
        for grid in self.grid.iter().chain(self.grids.iter()).chain(
            self.restoring_grid
                .iter()
                .filter_map(|restoring| restoring.grid.as_ref()),
        ) {
            receipts.extend(grid.resource_receipts()?);
        }
        if let Some(query) = &self.cell_word
            && let Some(receipt) = query.resource_receipt()?
        {
            receipts.push(receipt);
        }
        Ok(receipts)
    }

    /// Return original weak directory cursors only; the unique Producer retirement path still releases
    /// Source and Reader.
    pub(super) fn cancel_visual(&mut self) {
        // Ordinary layout also reaches this method every turn; an empty slot has no cursor to return
        // and needs no large Option move.
        // Actual cancellation takes the original slot before dropping its value, preserving release
        // order and reentrant visibility.
        if self.visual.is_some() {
            drop(self.visual.take());
        }
        if self.visual_word.is_some() {
            drop(self.visual_word.take());
        }
    }

    /// Alternate visual queries with original font/line work; a directory-read turn cannot also run a
    /// Composer quantum.
    fn advance_visual(
        &mut self,
        owner: &Owner,
        request: &NativeMarkdownPagedSceneRequest,
    ) -> Result<TargetAdvance> {
        let pending = owner.pending_word_delete.borrow().clone();
        let Some(pending) = pending.filter(|pending| {
            pending.visual_motion().is_some() || pending.visual_word_motion().is_some()
        }) else {
            self.cancel_visual();
            return Ok(TargetAdvance::VisibleBand);
        };
        let seed = pending.visual_seed.get().ok_or(ViewError::Revoked)?;
        let advance = if pending.visual_word_motion().is_some() {
            self.step_visual_word_intent(owner, request, seed)?
        } else {
            self.step_visual_intent(request, seed)?
        };
        match advance {
            VisualAdvance::Ready(ready) => {
                pending.visual_ready.set(Some(ready));
                Ok(TargetAdvance::Handled)
            }
            VisualAdvance::NeedPrefix => Ok(TargetAdvance::NeedPrefix),
            VisualAdvance::NeedsFoldExpansion(hidden) => {
                pending.fold_expansion.borrow_mut().replace(hidden);
                Ok(TargetAdvance::Handled)
            }
            VisualAdvance::Pending => {
                owner.schedule()?;
                Ok(TargetAdvance::Handled)
            }
        }
    }

    /// An actual visual-word query reuses new prefixes from the same writer without restarting
    /// classification while viewport preparation remains pending.
    fn step_visual_word_intent(
        &mut self,
        owner: &Owner,
        request: &NativeMarkdownPagedSceneRequest,
        seed: super::word::VisualIntent,
    ) -> Result<VisualAdvance> {
        let ticket =
            PreparationTicket::new(request, &self.source, self.serial, self.width, self.padding);
        ticket.require_pure(owner, &self.source)?;
        let Some(output) = self.output.as_ref() else {
            return Ok(VisualAdvance::NeedPrefix);
        };
        let mut output = output.borrow_mut();
        // Construct once from the original directed head, wrap affinity and Bidi side; later quanta reuse this exact weak query.
        if self.visual_word.is_none() {
            let affinity = if seed.upstream && seed.selection.head_utf16 != request.input.utf16_len
            {
                omni_markdown_core::MarkdownComposedLineAffinity::Upstream
            } else {
                omni_markdown_core::MarkdownComposedLineAffinity::Downstream
            };
            let side = seed
                .side
                .map(|side| omni_markdown_core::MarkdownComposedCaretSide {
                    level: side.level,
                    association: if side.logical_after {
                        omni_markdown_core::MarkdownComposedCaretAssociation::After
                    } else {
                        omni_markdown_core::MarkdownComposedCaretAssociation::Before
                    },
                });
            self.visual_word = Some(output.visual_word_query(
                seed.selection.head_utf16,
                affinity,
                side,
                seed.right,
            )?);
        }
        let step = output.step_visual_word(
            self.visual_word
                .as_mut()
                .expect("同一队首只装配一个原视觉词组查询"),
            MarkdownLineComposeBudget {
                bytes: 4096,
                scalars: 256,
                records: 256,
                metadata_units: 256,
            },
        )?;
        drop(output); // End original file/font query borrows before Native authority checks or selection publication.
        self.step_bytes = self
            .step_bytes
            .checked_add(step.bytes)
            .ok_or(Error::CounterExhausted)?;
        self.step_scalars = self
            .step_scalars
            .checked_add(step.scalars)
            .ok_or(Error::CounterExhausted)?;
        self.step_metadata = self
            .step_metadata
            .checked_add(step.metadata_units)
            .ok_or(Error::CounterExhausted)?;
        self.maximum_step_bytes = self.maximum_step_bytes.max(step.bytes);
        self.maximum_step_scalars = self.maximum_step_scalars.max(step.scalars);
        self.maximum_step_metadata = self.maximum_step_metadata.max(step.metadata_units);
        ticket.require(owner, &self.source)?;
        match step.answer {
            omni_markdown_core::MarkdownComposedVisualWordAnswer::Pending => {
                Ok(VisualAdvance::Pending)
            }
            omni_markdown_core::MarkdownComposedVisualWordAnswer::PrefixNotCovered => {
                // A turn that already read directory records yields first; only zero-work absence may request a Composer prefix now.
                Ok(if step.records == 0 {
                    VisualAdvance::NeedPrefix
                } else {
                    VisualAdvance::Pending
                })
            }
            omni_markdown_core::MarkdownComposedVisualWordAnswer::NeedsFoldExpansion(hidden) => {
                Ok(VisualAdvance::NeedsFoldExpansion(hidden))
            }
            omni_markdown_core::MarkdownComposedVisualWordAnswer::Found {
                utf16,
                side,
                line,
                ..
            } => Ok(VisualAdvance::Ready(super::word::VisualSelection {
                // Preserve the original anchor for extension and return the actual resolved Bidi side rather than inferring visual order.
                selection: omni_markdown_core::MarkdownTextSelection {
                    anchor_utf16: if seed.extend {
                        seed.selection.anchor_utf16
                    } else {
                        utf16
                    },
                    head_utf16: utf16,
                },
                upstream: Some(!line.is_empty() && utf16 == line.end && utf16 != 0),
                side: Some(
                    crate::native_surface::paged_input::NativeMarkdownBidiCaretSide {
                        level: side.level,
                        logical_after: side.association
                            == omni_markdown_core::MarkdownComposedCaretAssociation::After,
                    },
                ),
            })),
        }
    }

    /// Reuse this same query for immediate Ready navigation and queued continuation, without Native
    /// calls or scheduling.
    /// A Pending result leaves every already-read record in place, so entering the queue cannot restart
    /// its work.
    pub(super) fn step_visual_intent(
        &mut self,
        request: &NativeMarkdownPagedSceneRequest,
        seed: super::word::VisualIntent,
    ) -> Result<VisualAdvance> {
        self.target_source_authority(request, self.serial)?;
        let Some(output) = self.output.as_ref() else {
            return Ok(VisualAdvance::NeedPrefix);
        };
        // A nonextended selection collapses by the actual line base direction; caret motion otherwise
        // uses the weak visual query.
        let collapse = !seed.extend && seed.selection.anchor_utf16 != seed.selection.head_utf16;
        if self.visual.is_none() {
            let mut output = output.borrow_mut();
            let affinity = if seed.upstream && seed.selection.head_utf16 != request.input.utf16_len
            {
                omni_markdown_core::MarkdownComposedLineAffinity::Upstream
            } else {
                omni_markdown_core::MarkdownComposedLineAffinity::Downstream
            };
            self.visual = Some(if collapse {
                VisualWork {
                    query: None,
                    collapse: Some(output.line_lookup(
                        MarkdownComposedLineLookupTarget::Source {
                            utf16: seed.selection.head_utf16,
                            affinity,
                        },
                    )?),
                }
            } else {
                let side = seed
                    .side
                    .map(|side| omni_markdown_core::MarkdownComposedCaretSide {
                        level: side.level,
                        association: if side.logical_after {
                            omni_markdown_core::MarkdownComposedCaretAssociation::After
                        } else {
                            omni_markdown_core::MarkdownComposedCaretAssociation::Before
                        },
                    });
                VisualWork {
                    query: Some(output.visual_query(
                        seed.selection.head_utf16,
                        affinity,
                        side,
                        seed.right,
                    )?),
                    collapse: None,
                }
            });
        }
        let work = self.visual.as_mut().expect("原同Source视觉查询已装配");
        let (records, ready, missing) = {
            let mut output = output.borrow_mut();
            if let Some(lookup) = work.collapse.as_mut() {
                let step = output.step_line_lookup(lookup, 256)?;
                match step.answer {
                    MarkdownComposedLineLookupAnswer::Found(line) => {
                        // Collapse toward the actual visual side under this line base direction, without moving beyond the original selection.
                        let forward = seed.right == (line.base_level() == 0);
                        let head = if forward {
                            seed.selection.anchor_utf16.max(seed.selection.head_utf16)
                        } else {
                            seed.selection.anchor_utf16.min(seed.selection.head_utf16)
                        };
                        (
                            step.records,
                            Some(super::word::VisualSelection {
                                selection: omni_markdown_core::MarkdownTextSelection {
                                    anchor_utf16: head,
                                    head_utf16: head,
                                },
                                upstream: None,
                                side: None,
                            }),
                            false,
                        )
                    }
                    MarkdownComposedLineLookupAnswer::Pending => (step.records, None, false),
                    MarkdownComposedLineLookupAnswer::PrefixNotCovered => {
                        (step.records, None, true)
                    }
                }
            } else {
                let step = output
                    .step_visual(work.query.as_mut().expect("非收拢动作持原视觉游标"), 256)?;
                match step.answer {
                    omni_markdown_core::MarkdownComposedVisualAnswer::Found {
                        utf16,
                        side,
                        line,
                        ..
                    } => {
                        // A certified visual result supplies the new head and side; extending retains only the original anchor.
                        let selection = omni_markdown_core::MarkdownTextSelection {
                            anchor_utf16: if seed.extend {
                                seed.selection.anchor_utf16
                            } else {
                                utf16
                            },
                            head_utf16: utf16,
                        };
                        (
                            step.records,
                            Some(super::word::VisualSelection {
                                selection,
                                upstream: Some(!line.is_empty() && utf16 == line.end && utf16 != 0),
                                side: Some(crate::native_surface::paged_input::NativeMarkdownBidiCaretSide {
                                    level: side.level,
                                    logical_after: side.association == omni_markdown_core::MarkdownComposedCaretAssociation::After,
                                }),
                            }),
                            false,
                        )
                    }
                    omni_markdown_core::MarkdownComposedVisualAnswer::Pending => {
                        (step.records, None, false)
                    }
                    omni_markdown_core::MarkdownComposedVisualAnswer::PrefixNotCovered => {
                        (step.records, None, true)
                    }
                }
            }
        }; // End the reader borrow before scheduling, current checks or Native calls.
        self.target_source_health()?;
        if let Some(ready) = ready {
            return Ok(VisualAdvance::Ready(ready));
        }
        if missing && records == 0 {
            return Ok(VisualAdvance::NeedPrefix);
        } // Only a genuinely missing neighboring line requests original Composer work; it is not a document edge.
        Ok(VisualAdvance::Pending)
    }

    pub(super) fn advance(
        &mut self,
        owner: &Owner,
        request: &NativeMarkdownPagedSceneRequest,
    ) -> Result<()> {
        let ticket =
            PreparationTicket::new(request, &self.source, self.serial, self.width, self.padding);
        ticket.require_pure(owner, &self.source)?;
        let result = self.advance_inner(owner, request, &ticket);
        // Source failure precedes Native foreground revocation; replacing or cancelling an old Source
        // cannot hide its error.
        self.source
            .byte_offset_for_utf16(0)
            .map_err(ViewError::Document)?;
        match result {
            Err(cause) if cause.is_foreground_revocation() => {
                let current = ticket.current(owner, &self.source);
                self.source
                    .byte_offset_for_utf16(0)
                    .map_err(ViewError::Document)?;
                if current {
                    Err(cause) // With original foreground authority still valid, actual Native rejection cannot be blamed on a successor event.
                } else {
                    Err(Error::PreparationRevoked)
                }
            }
            Err(cause) => Err(cause),
            Ok(()) => ticket.require_pure(owner, &self.source),
        }
    }

    fn advance_inner(
        &mut self,
        owner: &Owner,
        request: &NativeMarkdownPagedSceneRequest,
        ticket: &PreparationTicket,
    ) -> Result<()> {
        self.source
            .byte_offset_for_utf16(0)
            .map_err(ViewError::Document)?;
        if !owner.current_body() {
            return Err(Error::Closed);
        }
        if self.cell_word.as_ref().is_some_and(|query| query.cancelled) {
            self.drain_cell_word(owner)?;
            owner.schedule()?;
            return Ok(()); // Ordinary layout also waits for the unique original Source slot before starting a successor CellInline.
        }
        if self.pending_line.is_some() {
            let mut parts = LINE_METRIC_BUDGET;
            self.advance_line(owner, &mut parts)?;
            owner.schedule()?;
            return Ok(());
        }
        if self.pending_batch.is_some() {
            self.advance_batch(owner)?;
            owner.schedule()?;
            return Ok(());
        }
        if self.queued_lines.is_some()
            && (!owner.source_target_pending()
                || self
                    .target
                    .as_ref()
                    .is_none_or(|target| !target.request.pending()))
        {
            // When a target cancels or is replaced, preserve completed LineEnds by checkpointing and
            // actually measuring them first,
            // before new targets or ordinary viewports can leave their unmeasured batch stranded behind
            // an old Ready prefix.
            self.checkpoint_batch()?;
            owner.schedule()?;
            return Ok(());
        }
        // Do not restart an in-flight trial; the unique worker delivers completion to the same weak
        // owner.
        if self.trial.is_some() {
            if self.advance_trial(owner, ticket)? {
                return Ok(());
            }
            owner.schedule()?;
            return Ok(());
        }
        if let Some(style) = self.pending_style.take() {
            let _ = self.start_style(owner, style, ticket)?;
            owner.schedule()?;
            return Ok(()); // This turn advances original semantics or font preparation only, without an additional Composer step.
        }
        if let Some(control) = self.pending_control.take() {
            self.provide_control(owner, control, ticket)?;
            owner.schedule()?;
            return Ok(()); // Control semantics and Native font preparation own this quantum exclusively, without further measurement in this turn.
        }
        // Retire completed rows after actual FontTrial return so a late result cannot lose its original
        // cell owner.
        // The independent grid slot retains unsubmitted rows and their original Core final-response
        // obligation even after backward scroll.
        self.grids.retain(|grid| {
            grid.visible(request)
                || self
                    .cell_word
                    .as_ref()
                    .is_some_and(|query| !query.cancelled && grid.word_row(query.target))
        });
        if self.restoring_grid.as_ref().is_some_and(|restoring| {
            let visible = restoring.top
                < request.virtual_viewport.document_origin.y + request.viewport.size.height
                && restoring.top + restoring.height > request.virtual_viewport.document_origin.y;
            !visible
                && !self.cell_word.as_ref().is_some_and(|query| {
                    !query.cancelled
                        && restoring
                            .grid
                            .as_ref()
                            .is_some_and(|grid| grid.word_row(query.target))
                })
                && restoring
                    .grid
                    .as_ref()
                    .is_none_or(|grid| !grid.has_preparation())
        }) {
            // Return this restoration when the viewport moves away, draining actual CellInline to its
            // original slot before retirement.
            self.restoring_grid = None;
        }
        if self.restoring_grid.is_some() {
            return self.advance_grid_restoration(owner, request, ticket);
        }
        let target = owner
            .source_target
            .borrow()
            .clone()
            .filter(|target| target.pending());
        if let Some(index) = self.grids.iter().position(|grid| {
            grid.needs_visible_work(request, self.padding)
                || target
                    .as_ref()
                    .is_some_and(|target| grid.needs_target(target))
        }) {
            let grid = self
                .grids
                .remove(index)
                .expect("真实可见行位置刚由原集合取得");
            self.restoring_grid = Some(GridRestoration {
                output: self.output.as_ref().expect("封存行仍属原正文输出").clone(),
                lookup: None,
                line: None,
                top: grid.top,
                height: grid.height(),
                grid: Some(grid),
            });
            return self.advance_grid_restoration(owner, request, ticket);
        }
        if self.grid.as_ref().is_some_and(|grid| !grid.submitted) {
            let grid = self.grid.as_ref().expect("唯一在途行仍保原提交请求");
            if !grid.visible(request)
                && !grid.has_preparation()
                && self.output.is_some()
                && !owner.source_target_pending()
                && owner.pending_page_scroll.borrow().is_none()
                && owner.pending_vertical.borrow().is_none()
                && owner.pending_word_delete.borrow().is_none()
            {
                // Ordinary backward scrolling need not finish a later in-flight row; explicit
                // navigation still obtains actual geometry through original work.
                // With no original CellInline/FontTrial outstanding, old-row restoration may briefly
                // borrow the same Source request.
                return self.publish(owner, request, ticket);
            }
            return self.advance_grid(owner, request, ticket);
        }
        // Resolve queued visual work before ordinary viewport publication; missing lines use this same
        // producer.
        let visual = self.advance_visual(owner, request)?;
        if matches!(visual, TargetAdvance::Handled) {
            return Ok(());
        }
        let vertical = self.advance_vertical_target(owner, request, ticket)?;
        if matches!(vertical, TargetAdvance::Handled) {
            return Ok(());
        }
        let target = self.advance_target(owner, request, ticket)?;
        if matches!(target, TargetAdvance::Handled) {
            return Ok(());
        }
        let needed = request.virtual_viewport.document_origin.y
            + request.viewport.size.height
            + self.padding;
        let page_scroll = owner
            .pending_page_scroll
            .borrow()
            .clone()
            .filter(|pending| {
                pending.matches(owner, request) && pending.source == self.source.authority()
            });
        let scroll_needed = page_scroll.as_ref().map(|pending| {
            needed
                + if pending.needs_next_page() {
                    request.viewport.size.height
                } else {
                    0.0
                }
        }); // Downward paging extends through current/next actual viewports only; existing prefix covers earlier rows without guessed system distance/overlap.
        if let Some(pending) = &page_scroll
            && scroll_needed.is_some_and(|needed| self.height >= needed || self.complete)
        {
            ticket.require(owner, &self.source)?;
            let viewport = owner.viewport.borrow().clone().ok_or(Error::Closed)?;
            if viewport.geometry()?.content_width != self.maximum_content_width {
                owner.native_preparation_geometry(&ticket.request, &viewport, || {
                    viewport.set_content_width(self.maximum_content_width)
                })?;
                owner.schedule()?;
                return Ok(()); // End this turn after horizontal-extent supersession; old authority cannot also change height or page.
            }
            if viewport.geometry()?.content_height != self.height() {
                owner.native_preparation_geometry(&ticket.request, &viewport, || {
                    viewport.set_content_height(self.height())
                })?;
                owner.schedule()?;
                return Ok(()); // Install actual prefix height first and end old geometry authority, avoiding premature system clamp at a short-prefix bottom.
            }
            let result = owner.consume_page_scroll(pending);
            self.target_source_health()?; // Source failure after Native/current reentry takes priority; a consumed action cannot be replayed.
            result?;
            return Ok(()); // Perform one standard Native action per turn; the original main-queue continuation owns remaining directional actions.
        }
        if !matches!(visual, TargetAdvance::NeedPrefix)
            && !matches!(target, TargetAdvance::NeedPrefix)
            && !matches!(vertical, TargetAdvance::NeedPrefix)
            && page_scroll.is_none()
            && self.cell_word.is_none()
            && self.lines != 0
            && (self.height >= needed || self.complete)
        {
            return self.publish(owner, request, ticket);
        }
        if self.complete {
            return Err(Error::Unsupported);
        }
        let quantum = MarkdownLineComposeBudget {
            bytes: 4096,
            scalars: 256,
            records: 256,
            metadata_units: 256,
        };
        let mut budget = quantum;
        // This turn owns one four-dimensional allowance; Core-certified Source-free directory work may
        // consume the remainder without renewal.
        // Source reads, context, shaping, and external work yield to original scheduling; bridge a
        // Source font-cache hit at most once.
        let mut cache_bridged = false;
        loop {
            ticket.require_pure(owner, &self.source)?;
            // Hold mutable Composer access for this Core call only, ending it before Native/Selection
            // reentry checks.
            let step = self.composer.as_mut().ok_or(Error::Closed)?.step(budget)?;
            self.step_bytes = self
                .step_bytes
                .checked_add(step.bytes)
                .ok_or(Error::CounterExhausted)?;
            self.step_scalars = self
                .step_scalars
                .checked_add(step.scalars)
                .ok_or(Error::CounterExhausted)?;
            self.step_metadata = self
                .step_metadata
                .checked_add(step.metadata_units)
                .ok_or(Error::CounterExhausted)?;
            budget.bytes = budget
                .bytes
                .checked_sub(step.bytes)
                .ok_or(Error::CounterExhausted)?;
            budget.scalars = budget
                .scalars
                .checked_sub(step.scalars)
                .ok_or(Error::CounterExhausted)?;
            budget.records = budget
                .records
                .checked_sub(step.records)
                .ok_or(Error::CounterExhausted)?;
            budget.metadata_units = budget
                .metadata_units
                .checked_sub(step.metadata_units)
                .ok_or(Error::CounterExhausted)?;
            // Observe actual cumulative work in this turn; the last step alone would hide
            // directory/scalar allowance already spent.
            self.maximum_step_bytes = self.maximum_step_bytes.max(quantum.bytes - budget.bytes);
            self.maximum_step_scalars = self
                .maximum_step_scalars
                .max(quantum.scalars - budget.scalars);
            self.maximum_step_metadata = self
                .maximum_step_metadata
                .max(quantum.metadata_units - budget.metadata_units);
            match step.answer {
                MarkdownLineComposeAnswer::Yielded => {
                    if self.semantics.is_some()
                        && step.metadata_units != 0
                        && let Some(style) = self
                            .composer
                            .as_ref()
                            .ok_or(Error::Closed)?
                            .pending_style_request()?
                    {
                        // Actual directory work may already have issued the original nonce request;
                        // accept that publication fact only,
                        // without stepping again for the same request or starting semantic reads/font
                        // work here.
                        self.pending_style = Some(style);
                    }
                    if self.pending_style.is_none()
                        && step.source_free_continuation
                        && !cache_bridged
                        && step.bytes == 0
                        && step.context_bytes == 0
                        && step.shapes == 0
                        && (step.scalars != 0 || step.records != 0 || step.metadata_units != 0)
                        && budget.scalars != 0
                        && budget.records != 0
                        && budget.metadata_units != 0
                    {
                        // Continue only Core-certified directory/measurement stages; zero bytes alone
                        // cannot prove absence of hidden Source reads.
                        // Every step rechecks original ticket/Source and strictly consumes at least one
                        // finite remaining budget; Source-byte allowance stays unused.
                        continue;
                    }
                    owner.schedule()?;
                }
                MarkdownLineComposeAnswer::NeedStyle(request) => {
                    if self.semantics.is_some()
                        || cache_bridged
                        || step.bytes != 0
                        || step.scalars != 0
                        || step.records != 0
                        || step.metadata_units != 0
                        || budget.bytes < super::font_choice::MAXIMUM_CHOICE_SOURCE_BYTES
                    {
                        self.pending_style = Some(request);
                        // Preview font decisions may advance original parser/metadata; this bridge
                        // never treats them as free styles.
                        // If Core performed work or this turn already bridged, defer the original
                        // request without renewing a full allowance.
                        owner.schedule()?;
                    } else if let StyleStart::Cached { source_bytes } =
                        self.start_style(owner, request, ticket)?
                    {
                        // Key reads already entered trial_source_bytes; subtract shared allowance only
                        // without counting them again.
                        // Reach here only after zero work from this Core step; prior directory costs
                        // stay deducted and no remainder is renewed.
                        budget.bytes = budget
                            .bytes
                            .checked_sub(source_bytes)
                            .ok_or(Error::CounterExhausted)?;
                        self.maximum_step_bytes =
                            self.maximum_step_bytes.max(quantum.bytes - budget.bytes);
                        cache_bridged = true;
                        // After key Source reads, allow the original bridge single next step only,
                        // without merging a later Yielded into this turn.
                        continue;
                    } else if !self.has_in_flight() {
                        // Without an in-flight worker, Pending needs the original Body continuation
                        // rather than a notification that cannot arrive.
                        // Existing same-serial scheduling coalesces as usual, without polling or a
                        // second font trial.
                        owner.schedule()?;
                    }
                    // The original worker/continuation owns Pending wakeup; do not advance trials again
                    // or spin-poll in this turn.
                }
                MarkdownLineComposeAnswer::LineReady(facts) => {
                    self.accept_line(owner, &facts)?;
                    owner.schedule()?;
                }
                MarkdownLineComposeAnswer::Complete => {
                    self.complete = true;
                    owner.schedule()?;
                }
                MarkdownLineComposeAnswer::NeedsControl(request) => {
                    if self.semantics.is_some() {
                        self.pending_control = Some(request);
                    } else {
                        // Source and Cell retain their original synchronous control response, font
                        // choice, and scheduling cadence.
                        let fonts = owner.fonts.borrow().clone();
                        let font = fonts.first().ok_or(Error::Closed)?;
                        let response = request.respond(
                            font.registered(),
                            font.native().point_size() as f32,
                            owner.paragraph.core_style().clone(),
                        )?;
                        self.composer
                            .as_mut()
                            .ok_or(Error::Closed)?
                            .provide_control(response)?;
                    }
                    owner.schedule()?;
                }
                // Unsupported controls, dictionary resolution, Tab placement, or missing frontier proof
                // cannot become permanent Pending,
                // or fall back to whole-paragraph TextKit reads; fail this candidate explicitly without
                // switching the production backend.
                MarkdownLineComposeAnswer::NeedsResolution(_reason) => {
                    #[cfg(feature = "native-markdown-runtime-probe")]
                    eprintln!(
                        "native_body_unsupported phase=resolution serial={} reason={_reason:?}",
                        self.serial
                    ); // Log original Core ranges/reasons only, without text; actual missing glyphs cannot become generic Pending.
                    return Err(Error::Unsupported);
                }
                MarkdownLineComposeAnswer::NeedsTabPlacement(_reason) => {
                    #[cfg(feature = "native-markdown-runtime-probe")]
                    eprintln!(
                        "native_body_unsupported phase=tab serial={} reason={_reason:?}",
                        self.serial
                    );
                    return Err(Error::Unsupported);
                }
                MarkdownLineComposeAnswer::NeedsFrontier(_reason) => {
                    #[cfg(feature = "native-markdown-runtime-probe")]
                    eprintln!(
                        "native_body_unsupported phase=frontier serial={} reason={_reason:?}",
                        self.serial
                    );
                    return Err(Error::Unsupported);
                }
                MarkdownLineComposeAnswer::NeedsTableRow(_) => return Err(Error::Unsupported),
            }
            break;
        }
        Ok(())
    }

    /// Serialize cell work with the body Composer: one original quantum or actual font-preparation step
    /// per turn.
    fn advance_grid(
        &mut self,
        owner: &Owner,
        request: &NativeMarkdownPagedSceneRequest,
        ticket: &PreparationTicket,
    ) -> Result<()> {
        let working = match self.restoring_grid.as_mut() {
            Some(restoring) => restoring.grid.as_mut().expect("恢复来源完成后才推进格字形"),
            None => self.grid.as_mut().expect("当前表格作业已存在"),
        };
        let advanced = working.advance(
            owner,
            &self.source,
            request,
            self.cell_word
                .as_ref()
                .filter(|query| !query.cancelled)
                .map(|query| query.target),
            &mut self.table_widths,
            self.semantics
                .as_mut()
                .expect("Grid只属于原Preview语义请求"),
        );
        // Collect performed work before reporting resource-operation failure so error paths cannot omit
        // actual reads.
        let measured_width = working.width();
        let work = working.take_work();
        self.record_grid_work(work)?;
        let step = advanced?;
        if measured_width > 0.0 {
            // Solved quantized prefix widths may authorize horizontal extents, without row-height or
            // Complete authority.
            self.maximum_content_width = self
                .maximum_content_width
                .max(measured_width + self.padding * 2.0);
        }
        match step {
            grid::Advance::Progress => owner.schedule()?,
            grid::Advance::Style { column, request } => {
                // Cell display coordinates answer that original cell Composer only; font trials still
                // exclusively use the same Body slot.
                let display = self.active_grid().display(column);
                let row = self.active_grid().source_range().start;
                let selection = super::semantics::Selection::cell(owner, display, &request)?;
                let _ = self.start_font_trial(
                    owner,
                    request,
                    selection,
                    FontTrialTarget::Cell { row, column },
                    ticket,
                )?;
                if !self.has_in_flight() {
                    owner.schedule()?;
                }
            }
            grid::Advance::Control { column, request } => {
                // Whitespace/newline controls also validate the same display authority; body control
                // authority cannot answer a cell Source.
                let display = self.active_grid().display(column);
                let mut selection =
                    super::semantics::Selection::cell_control(owner, display.clone(), &request)?;
                selection.require_control(owner, display.source(), &request)?;
                let index = selection.base_index(owner, &self.source, ticket)?;
                ticket.require(owner, &self.source)?;
                let fonts = owner.fonts.borrow().clone();
                let response = request
                    .respond(
                        fonts[index].registered(),
                        fonts[index].native().point_size() as f32,
                        owner.paragraph.core_style().clone(),
                    )?
                    .with_presentation(selection.styles(), 0)?;
                self.active_grid_mut()
                    .composer(column)
                    .provide_control(response)?;
                owner.schedule()?;
            }
            grid::Advance::Visible if self.restoring_grid.is_some() => {
                self.finish_grid_restoration();
                owner.schedule()?;
            }
            grid::Advance::Visible => self.publish(owner, request, ticket)?,
            grid::Advance::Complete if self.restoring_grid.is_some() => {
                // The old row is already sealed; restored cells return to current visible rows without
                // another Core submission.
                self.finish_grid_restoration();
                owner.schedule()?;
            }
            grid::Advance::Complete => {
                // Answer the block row after all cells actually end; submitted means Core received the
                // response, not that LineEnd sealed.
                let response = self
                    .grid
                    .as_ref()
                    .expect("完成行保同一request")
                    .response(owner)?;
                self.composer
                    .as_mut()
                    .expect("原正文Composer仍暂停同一TableRow")
                    .provide_table_row(response)?;
                self.grid.as_mut().expect("只提交当前行").submitted = true;
                owner.schedule()?;
            }
            grid::Advance::Target {
                request: target,
                rect,
            } => {
                let viewport = owner.viewport.borrow().clone().ok_or(Error::Closed)?;
                if viewport.geometry()?.content_width != self.maximum_content_width {
                    owner.native_preparation_geometry(request, &viewport, || {
                        viewport.set_content_width(self.maximum_content_width)
                    })?;
                    owner.schedule()?;
                    return Ok(()); // Install actual column-prefix document extents before Native can clamp a right-column target back to the first column.
                }
                if viewport.geometry()?.content_height != self.height() {
                    owner.native_preparation_geometry(request, &viewport, || {
                        viewport.set_content_height(self.height())
                    })?;
                    owner.schedule()?;
                    return Ok(()); // Install actual cell-prefix document height before old short-content geometry can clamp away the target.
                }
                ticket.require(owner, &self.source)?;
                // Guard actual Native reveal with original Find liveness so revoked targets cannot
                // change the current document viewport.
                let revealed = owner.native_preparation_geometry(request, &viewport, || {
                    viewport.reveal_document_rect(rect, &|| target.current_native(owner))
                });
                self.source
                    .byte_offset_for_utf16(0)
                    .map_err(ViewError::Document)?;
                revealed?;
                owner.schedule()?;
            }
        }
        Ok(())
    }

    /// The unique active cell work belongs to current/restoring rows, sharing the original FontTrial
    /// and serial semantic slot.
    fn active_grid(&self) -> &Grid {
        match &self.restoring_grid {
            Some(restoring) => restoring.grid.as_ref().expect("恢复格已持实际来源"),
            None => self.grid.as_ref().expect("当前行作业仍在原槽"),
        }
    }
    /// Return controls to their original in-flight cell rather than another visible-row receiver.
    fn active_grid_mut(&mut self) -> &mut Grid {
        match &mut self.restoring_grid {
            Some(restoring) => restoring.grid.as_mut().expect("恢复格已持实际来源"),
            None => self.grid.as_mut().expect("当前行作业仍在原槽"),
        }
    }

    /// Restoration retains current visible cells only; its query Reader/handle retire on transfer
    /// without historical inventory.
    fn finish_grid_restoration(&mut self) {
        let restoring = self.restoring_grid.take().expect("只有实际恢复作业能完成");
        self.grids
            .push_back(restoring.grid.expect("实际格前缀已经准备完成"));
    }

    /// Each turn performs original directory lookup, one semantic quantum, or one cell step without a
    /// second Source graph.
    fn advance_grid_restoration(
        &mut self,
        owner: &Owner,
        request: &NativeMarkdownPagedSceneRequest,
        ticket: &PreparationTicket,
    ) -> Result<()> {
        if self
            .restoring_grid
            .as_ref()
            .expect("实际恢复续体已存在")
            .grid
            .is_some()
        {
            return self.advance_grid(owner, request, ticket);
        }
        let restoring = self.restoring_grid.as_mut().expect("实际恢复续体仍在原槽");
        if let Some(lookup) = restoring.lookup.as_mut() {
            // The original Reader issues handles; an ordinal is a query coordinate and cannot grant
            // table Source authority to bare ranges.
            let step = restoring
                .output
                .borrow_mut()
                .step_line_lookup(lookup, METADATA_PER_TURN)?;
            match step.answer {
                MarkdownComposedLineLookupAnswer::Pending => {}
                MarkdownComposedLineLookupAnswer::Found(line) => {
                    restoring.line = Some(line);
                    restoring.lookup = None;
                }
                MarkdownComposedLineLookupAnswer::PrefixNotCovered => {
                    return Err(Error::Unsupported);
                }
            }
            owner.schedule()?;
            return Ok(());
        }
        let line = restoring.line.as_ref().expect("原行查找已取得真实 handle");
        let document = owner
            .document
            .borrow()
            .as_ref()
            .expect("恢复沿原 Body 文档")
            .clone();
        let document = document.borrow();
        let Some(row) = self
            .semantics
            .as_mut()
            .expect("恢复只属于预览原来源请求")
            .table_row(&document, &self.source, line.facts().source_utf16.start)?
        else {
            owner.schedule()?;
            return Ok(());
        };
        // Revalidate the sealed handle with its actual Reader; widths/heights cannot come from another
        // output or ordinary text line.
        let mut widths = [0i64; 256];
        let columns = restoring.output.borrow_mut().table_column_widths(
            &self.source.authority(),
            line,
            &mut widths,
        )?;
        if row.source_range() != line.facts().source_utf16 || row.columns() != columns {
            return Err(ViewError::Revoked.into());
        }
        let widths = widths[..columns]
            .iter()
            .map(|width| *width as f64 / 64.0)
            .collect::<Vec<_>>()
            .into();
        restoring.grid = Some(Grid::restored(
            row,
            restoring.top,
            line.facts().y_advance_26_6 as f64 / 64.0,
            widths,
        ));
        ticket.require_pure(owner, &self.source)?;
        owner.schedule()
    }

    /// The original Producer takes this cell-work ledger, preserving actual performed reads after every
    /// cell/natural-width task retires.
    fn record_grid_work(&mut self, work: grid::Work) -> Result<()> {
        add_semantic_work(&mut self.cell_display_work, work.display)?;
        for (total, value) in [
            (&mut self.step_bytes, work.bytes),
            (&mut self.step_scalars, work.scalars),
            (&mut self.step_metadata, work.metadata),
            (
                &mut self.cell_frontier_source_bytes,
                work.frontier_source_bytes,
            ),
            (&mut self.cell_shaped_input_bytes, work.shaped_input_bytes),
        ] {
            // Totals retain retired-cell actual costs; fail overflow instead of saturating into
            // apparently compliant values.
            *total = total.checked_add(value).ok_or(Error::CounterExhausted)?;
        }
        self.maximum_step_bytes = self.maximum_step_bytes.max(work.maximum_bytes);
        self.maximum_step_scalars = self.maximum_step_scalars.max(work.maximum_scalars);
        self.maximum_step_metadata = self.maximum_step_metadata.max(work.maximum_metadata);
        Ok(())
    }

    /// Original NeedStyle receives ordinary font style or an actual structural row; tables do not use
    /// ordinary paragraph font responses.
    fn start_style(
        &mut self,
        owner: &Owner,
        request: MarkdownLineStyleRequest,
        ticket: &PreparationTicket,
    ) -> Result<StyleStart> {
        if let Some(boundary) = self
            .composer
            .as_ref()
            .expect("正文样式请求持原Composer")
            .root_leaf_style_boundary_for(&request)?
        {
            // The original left semantic publication borrows one adjacent right grapheme as font
            // evidence without advancing its semantic window,
            // or transferring right-side style, coordinates, or input authority into left-side output.
            let selection = super::semantics::Selection::root_leaf_boundary(
                owner,
                &self.source,
                &request,
                boundary,
                ticket,
            )?;
            return self.start_font_trial(owner, request, selection, FontTrialTarget::Body, ticket);
        }
        let selection = if let Some(semantic) = self.semantics.as_mut() {
            match semantic.select(owner, &self.source, &request, ticket)? {
                Some(super::semantics::StyleDecision::Font(selection)) => selection,
                Some(super::semantics::StyleDecision::Table(row)) => {
                    if self.queued_lines.is_some() {
                        // Distant selection/reveal may stage actual LineEnds in a small batch before
                        // height accounts for them.
                        // Cell top must use the prefix with complete original Native metrics rather
                        // than opening the next row at old height,
                        // otherwise later total-height corrections leave this row text/click box
                        // overlapping the previous table row.
                        self.pending_style = Some(request);
                        self.checkpoint_batch()?;
                        owner.schedule()?;
                        return Ok(StyleStart::Pending); // The original NeedStyle/nonce remains unconsumed; measure its pending batch in actual quanta first.
                    }
                    // Core checks actual cursor, original nonce, and row range so this handoff cannot
                    // discard an in-flight prefix.
                    let request = self
                        .composer
                        .as_mut()
                        .expect("原NeedStyle持同一Composer")
                        .begin_table_row(&request, row)?;
                    self.grid = Some(Grid::new(
                        request,
                        self.height + self.padding,
                        &self.table_widths,
                    ));
                    return Ok(StyleStart::Pending); // Block handoff creates no font response or fabricated LineReady; later turns advance actual cells.
                }
                None => {
                    // While original parser publication is incomplete, retain its NeedStyle
                    // continuation without default semantics.
                    self.pending_style = Some(request);
                    return Ok(StyleStart::Pending);
                }
            }
        } else {
            let points = owner.fonts.borrow()[0].native().point_size() as f32;
            super::semantics::Selection::literal(&self.source, &request, points)
        };
        self.start_font_trial(owner, request, selection, FontTrialTarget::Body, ticket)
    }

    /// Both actual coordinate domains share one trial slot/worker; outer PreparationTicket always
    /// validates canonical Body authority.
    fn start_font_trial(
        &mut self,
        owner: &Owner,
        request: MarkdownLineStyleRequest,
        mut selection: super::semantics::Selection,
        target: FontTrialTarget,
        ticket: &PreparationTicket,
    ) -> Result<StyleStart> {
        let layout_source = self.layout_source(target).clone();
        // Inner layout authority validates display coordinates while the outer ticket validates the
        // canonical document; both remain necessary.
        selection.require(owner, &layout_source, &request)?;
        let semantic_base = if self.semantics.is_some() {
            selection.base_index(owner, &self.source, ticket)?
        } else {
            0
        };
        let probe = self.layout_composer(target).font_probe(&request)?;
        if probe.source_range().len() > 4096 {
            // Fallback probes accept original bounded slices only; oversized requests cannot fall back
            // to whole-cell string measurement.
            return Err(Error::Unsupported);
        }
        // Use one actual Base and end the palette borrow after cloning its Rc, without copying the
        // entire vector per style request
        // or retaining unrelated fonts. The original Base spans subsequent validation and Native
        // trials.
        let base = Rc::clone(&owner.fonts.borrow()[semantic_base]);
        // The continuation belongs to Core current shaping segment and cannot replace semantic font
        // preference.
        // Try every subsequent complete grapheme in original semantic Base, system for_string, then
        // system-cascade order;
        // return to Base when available again to preserve mixed Chinese/English space widths and
        // original wrapping.
        selection.style.points = base.native().point_size() as f32;
        // Key capture reads the original Core probe, certified byte envelope, and captured font
        // scalars, without Native calls.
        // Original pure checks suffice for Input/Source here, without repeated full AppKit geometry
        // reads for these pure validations.
        // Original capture retains pre/post Source/scope checks; on a hit the following ticket.require
        // again
        // validates full Native geometry, while fallback still guards every Native step with original
        // authority across reentry.
        let (read, key) =
            super::font_choice::FontChoiceKey::capture(&probe, &selection.style, &base, &|| {
                ticket.pure_current(owner, &self.source)
            });
        // Bounded key reads are actual work even if post-call Source/cancellation/scope checks reject
        // the result.
        self.trial_source_bytes = self
            .trial_source_bytes
            .checked_add(read)
            .ok_or(Error::CounterExhausted)?;
        let key = key?;
        let cached = key
            .as_ref()
            .and_then(|key| owner.font_choices.borrow().get(key));
        if let Some(font) = cached {
            // Reuse choices for the same complete grapheme, context, and original font options only
            // while current original authority/semantics remain valid.
            // A hit creates no TrialResult and uses neither old grapheme, publication, nor font
            // directory as this request authority.
            selection.require(owner, &layout_source, &request)?;
            ticket.require(owner, &self.source)?;
            probe.validate()?;
            let response =
                selected_font_response(owner, &layout_source, request, selection, &base, &font)?;
            self.layout_composer(target).provide_style(response)?;
            // A cache hit has no worker event; the caller advances Core within remaining budget or
            // schedules at the original Grid/semantic boundary.
            // Return actual reads only, without retaining probe/Selection/response or claiming a
            // background trial result.
            return Ok(StyleStart::Cached { source_bytes: read });
        }
        let (fallback, pending_probe) = if probe.shaping_segment_ready()? {
            let fallback = NativeMarkdownFontFallback::new_with_probe(
                probe,
                base.native(),
                &base.native().core_font().authority(),
                NativeMarkdownProbedFontFallbackRequest {
                    style: selection.style.clone(),
                    languages: &["zh-Hans", "en"],
                    cascade_budget: NativeMarkdownFontCascadeBudget::default(),
                    maximum_font_bytes: owner.configuration.maximum_font_bytes,
                },
                owner.mtm,
            )?;
            (Some(fallback), None)
        } else {
            (None, Some(probe)) // The unique original FontTrial waits for its first segment without extra font capture or another worker.
        };
        self.trial = Some(FontTrial {
            target,
            request,
            fallback,
            pending_probe,
            selection,
            in_flight: false,
            result: None,
            base: base.clone(),
            cache_key: key,
        });
        self.advance_trial(owner, ticket)?;
        Ok(StyleStart::Pending)
    }

    /// Route to the existing concrete body/current cell only, without default Source or successor
    /// assembly.
    fn layout_source(&self, target: FontTrialTarget) -> &MarkdownLayoutTextSource {
        match target {
            FontTrialTarget::Body => &self.source,
            FontTrialTarget::Cell { row, column } => self.grid_for_row(row).source(column),
        }
    }
    /// Return responses to their original issuing Composer; late cell trials cannot answer the current
    /// body Composer.
    fn layout_composer(&mut self, target: FontTrialTarget) -> &mut MarkdownLineComposer {
        match target {
            FontTrialTarget::Body => self.composer.as_mut().expect("Body试次持原Composer"),
            FontTrialTarget::Cell { row, column } => self.grid_for_row_mut(row).composer(column),
        }
    }

    /// Route actual trials by original physical row; restored visible rows borrow this existing work
    /// without Source recapture.
    fn grid_for_row(&self, row: usize) -> &Grid {
        self.grid
            .iter()
            .chain(self.grids.iter())
            .chain(
                self.restoring_grid
                    .iter()
                    .filter_map(|restoring| restoring.grid.as_ref()),
            )
            .find(|grid| grid.source_range().start == row)
            .expect("Cell试次始终持原Grid作业")
    }
    /// Mutable routing also uses original row start; an assembly error panics rather than choosing
    /// another default cell receiver.
    fn grid_for_row_mut(&mut self, row: usize) -> &mut Grid {
        self.grid
            .iter_mut()
            .chain(self.grids.iter_mut())
            .chain(
                self.restoring_grid
                    .iter_mut()
                    .filter_map(|restoring| restoring.grid.as_mut()),
            )
            .find(|grid| grid.source_range().start == row)
            .expect("Cell试次始终持原Grid作业")
    }

    /// Source retains original control fonts; preview controls acquire actual semantics first so LF
    /// cannot expand hidden fences into normal lines.
    fn provide_control(
        &mut self,
        owner: &Owner,
        request: omni_markdown_core::MarkdownLineInlineControl,
        ticket: &PreparationTicket,
    ) -> Result<()> {
        let selection =
            if request.kind == omni_markdown_core::MarkdownLineInlineControlKind::EmptyParagraph {
                None // An actual empty EOF paragraph has no Markdown content and uses the literal font path.
            } else if let Some(semantics) = self.semantics.as_mut() {
                match semantics.select_control(owner, &self.source, &request, ticket)? {
                    Some(selection) => Some(selection),
                    None => {
                        self.pending_control = Some(request);
                        return Ok(());
                    }
                }
            } else {
                None
            };
        let mut selection = selection;
        let index = if let Some(selection) = selection.as_mut() {
            selection.require_control(owner, &self.source, &request)?;
            selection.base_index(owner, &self.source, ticket)?
        } else {
            0
        };
        ticket.require(owner, &self.source)?;
        // Controls need the originally selected font only; an independent Rc retains it through
        // response delivery without a live RefCell borrow.
        // The original Body keeps other palette fonts, without another temporary vector for this
        // control.
        let font = Rc::clone(&owner.fonts.borrow()[index]);
        let mut response = request.clone().respond(
            font.registered(),
            font.native().point_size() as f32,
            owner.paragraph.core_style().clone(),
        )?;
        if let Some(selection) = selection {
            selection.require_control(owner, &self.source, &request)?;
            let styles = selection.styles();
            let inset =
                crate::native_surface::preview_paragraph::PreviewParagraph::from_styles(styles)
                    .map_or(0, |paragraph| paragraph.inset_26_6());
            response = response.with_presentation(styles, inset)?;
        }
        self.composer
            .as_mut()
            .ok_or(Error::Closed)?
            .provide_control(response)?;
        Ok(())
    }

    /// Resume one original trial, transferring work to the sole worker only after its exact shaping
    /// segment is ready.
    fn advance_trial(&mut self, owner: &Owner, ticket: &PreparationTicket) -> Result<bool> {
        let mut trial = self.trial.take().ok_or(Error::Closed)?;
        if trial.selection.context_changed(owner)
            || (trial.fallback.is_none() && trial.pending_probe.is_none())
        {
            // After original selection changes, cancel the old trial before a new publication can
            // certify its old result.
            // The same worker returns the in-flight packet for destruction; drain it before sending
            // another trial with ambiguous serial ownership.
            if let Some(fallback) = trial.fallback.take() {
                fallback.close();
            }
            drop(trial.pending_probe.take()); // The first-segment wait belongs to original scope only, without transfer to a new selection.
            if trial.in_flight {
                self.trial = Some(trial);
                return Ok(true);
            }
            let retired = trial
                .result
                .take()
                .map_or(Ok(()), |result| result.retire(false).map_err(Error::from));
            if let FontTrialTarget::Body = trial.target {
                self.pending_style = Some(trial.request);
            } // The same Body resets the old cell selection/Grid; never place a stale cell-local request into the body slot.
            retired?; // Even with changed context, the next style request cannot swallow actual old-trial cleanup failure.
            return Ok(false);
        }
        let layout_source = self.layout_source(trial.target).clone();
        trial
            .selection
            .require(owner, &layout_source, &trial.request)?;
        if trial.in_flight {
            self.trial = Some(trial);
            return Ok(true);
        }
        let fonts = owner.fonts.borrow().clone();
        let base = trial.base.clone();
        let authority = base.native().core_font().authority();
        let source = self.source.clone();
        let current = || {
            if !ticket.current(owner, &source) {
                return false;
            }
            ticket.current(owner, &source)
        };
        if let Some(mut probe) = trial.pending_probe.take() {
            let step = self.layout_composer(trial.target).step_font_probe_segment(
                &mut probe,
                MarkdownLineComposeBudget {
                    bytes: 4096,
                    scalars: 256,
                    records: 256,
                    metadata_units: 256,
                },
            )?;
            self.record_font_probe_work(&step)?;
            trial
                .selection
                .require(owner, &layout_source, &trial.request)?;
            ticket.require(owner, &self.source)?;
            match step.answer {
                MarkdownLineFontProbeSegmentAnswer::Pending => {
                    trial.pending_probe = Some(probe);
                    self.trial = Some(trial);
                    owner.schedule()?;
                    return Ok(false);
                }
                MarkdownLineFontProbeSegmentAnswer::Ready => {
                    trial.fallback = Some(NativeMarkdownFontFallback::new_with_probe(
                        probe,
                        base.native(),
                        &authority,
                        NativeMarkdownProbedFontFallbackRequest {
                            style: trial.selection.style.clone(),
                            languages: &["zh-Hans", "en"],
                            cascade_budget: NativeMarkdownFontCascadeBudget::default(),
                            maximum_font_bytes: owner.configuration.maximum_font_bytes,
                        },
                        owner.mtm,
                    )?);
                }
                MarkdownLineFontProbeSegmentAnswer::Complete => {
                    return Err(crate::native_surface::paged_font_fallback::NativeMarkdownFontFallbackError::InvalidState.into());
                }
            }
        }
        if let Some(result) = trial.result.take() {
            if let Ok(facts) = result.outcome() {
                self.trial_source_bytes = self
                    .trial_source_bytes
                    .checked_add(facts.source_bytes_read)
                    .ok_or(Error::CounterExhausted)?;
            }
            match trial
                .fallback
                .as_mut()
                .expect("活试次持原fallback")
                .complete_trial_probed(base.native(), &authority, result, &current)?
            {
                Some(NativeMarkdownFontFallbackCompletion::Rejected { .. }) => {
                    // After actual Base missing-glyph rejection, this owner tries system for_string
                    // before original cascade order.
                    // Reuse exact palette captures only after selecting an actual candidate, without
                    // trying hidden/other-size fonts over body text.
                    trial
                        .fallback
                        .as_mut()
                        .expect("缺字后仍持原候选 owner")
                        .restart_probe(
                            self.layout_composer(trial.target),
                            base.native(),
                            &authority,
                            &current,
                        )?;
                    self.trial = Some(trial);
                    owner.schedule()?;
                    return Ok(false);
                }
                Some(NativeMarkdownFontFallbackCompletion::Selected(choice)) => {
                    self.accept_font_choice(owner, trial, choice, ticket, &current)?;
                    return Ok(false);
                }
                None => {} // This actual segment passed and released, but the original coordinator still must confirm subsequent segments or whole-grapheme completion.
            }
        }
        let fallback = trial.fallback.as_mut().expect("活试次持原fallback");
        let (step, advanced) = fallback.step_probe_segment(
            self.layout_composer(trial.target),
            &trial.selection.style,
            MarkdownLineComposeBudget {
                bytes: 4096,
                scalars: 256,
                records: 256,
                metadata_units: 256,
            },
            base.native(),
            &authority,
            &current,
        );
        if let Some(step) = &step {
            self.record_font_probe_work(step)?; // Post-call cancellation still retains actual metadata/read/scalar quantum costs.
        }
        match advanced? {
            Some(NativeMarkdownFontFallbackCompletion::Selected(choice)) => {
                self.accept_font_choice(owner, trial, choice, ticket, &current)?;
                return Ok(false);
            }
            Some(NativeMarkdownFontFallbackCompletion::Rejected { .. }) => {
                return Err(crate::native_surface::paged_font_fallback::NativeMarkdownFontFallbackError::InvalidState.into());
            }
            None => {}
        }
        if !fallback.probe_segment_ready()? {
            self.trial = Some(trial);
            owner.schedule()?;
            return Ok(false); // Wait on the same candidate/original probe without submitting an empty Job or skipping to another font.
        }
        let next = fallback.next_trial_probed(base.native(), &authority, &fonts, &current);
        // Reads precede candidate construction/post-call authority checks, so failure and unavailable
        // captures still account performed work.
        self.trial_source_bytes = self
            .trial_source_bytes
            .checked_add(fallback.take_probe_source_bytes())
            .ok_or(Error::CounterExhausted)?;
        match next? {
            NativeMarkdownFontFallbackStep::Trial(packet) => {
                if owner.worker.borrow().is_none() {
                    *owner.worker.borrow_mut() = Some(FontWorker::new(owner)?);
                }
                owner
                    .worker
                    .borrow()
                    .as_ref()
                    .ok_or(Error::Closed)?
                    .submit(owner.serial.get(), *packet)?;
                trial.in_flight = true;
                self.trial = Some(trial);
                Ok(true)
            }
            NativeMarkdownFontFallbackStep::UnavailableCandidate { .. }
            | NativeMarkdownFontFallbackStep::UnavailableForString { .. } => {
                self.trial = Some(trial);
                owner.schedule()?;
                Ok(false)
            }
            NativeMarkdownFontFallbackStep::PendingDependencies => {
                // Undelivered font dependencies certify neither missing glyphs nor Ready; diagnostics
                // record phase only without text.
                #[cfg(feature = "native-markdown-runtime-probe")]
                eprintln!("native_body_font_trial outcome=pending_dependencies");
                Err(Error::Unsupported)
            }
            NativeMarkdownFontFallbackStep::Exhausted { tried } => {
                // Report exhaustion only after trying the actual ordered candidates, without
                // substituting a default font for unknown glyphs.
                #[cfg(feature = "native-markdown-runtime-probe")]
                eprintln!("native_body_font_trial outcome=exhausted actual_tried={tried}");
                #[cfg(not(feature = "native-markdown-runtime-probe"))]
                let _ = tried;
                Err(Error::Unsupported)
            }
        }
    }

    /// Account metadata continuation in the same body/cell work ledger, without treating
    /// multisegment-boundary lookup as free font choice.
    fn record_font_probe_work(&mut self, step: &MarkdownLineFontProbeSegmentStep) -> Result<()> {
        for (total, value) in [
            (&mut self.step_bytes, step.bytes),
            (&mut self.step_scalars, step.scalars),
            (&mut self.step_metadata, step.metadata_units),
        ] {
            *total = total.checked_add(value).ok_or(Error::CounterExhausted)?;
        }
        self.maximum_step_bytes = self.maximum_step_bytes.max(step.bytes);
        self.maximum_step_scalars = self.maximum_step_scalars.max(step.scalars);
        self.maximum_step_metadata = self.maximum_step_metadata.max(step.metadata_units);
        Ok(())
    }

    /// Commit through the original response after actual whole-grapheme completion; single/multiple
    /// segments share registration, Source, and cache ordering.
    fn accept_font_choice(
        &mut self,
        owner: &Owner,
        trial: FontTrial,
        choice: NativeMarkdownFontChoice,
        ticket: &PreparationTicket,
        current: &impl Fn() -> bool,
    ) -> Result<()> {
        let layout_source = self.layout_source(trial.target).clone();
        trial
            .selection
            .require(owner, &layout_source, &trial.request)?;
        let fonts = owner.fonts.borrow().clone();
        let base = trial.base.clone();
        #[cfg(feature = "native-markdown-runtime-probe")]
        let actual_choice = (choice.source_range(), choice.origin(), choice.facts());
        // Actual complete trials select this grapheme font; returning to semantic Base does not require
        // prior fallback failure.
        // Original responses and Core FontRuns validate/merge same-Source, same-style registered fonts.
        let selected = choice.into_captured_font(base.native(), &layout_source, current)?;
        let font = if let Some(selected) = selected {
            if let Some(existing) = fonts.iter().find(|font| {
                selected
                    .core_font()
                    .permits(&font.native().core_font().authority())
            }) {
                // Before file snapshotting, the actual system candidate was verified as the same font;
                // reuse original Core registration identity only here.
                // The unique worker still tested the complete original grapheme; support for another
                // character cannot skip this trial.
                // Return this temporary strong claim while Body keeps unique original
                // registration/metrics, without duplicate insertion during cell scrolling.
                let reused = existing.clone();
                drop(selected);
                reused
            } else {
                let selected =
                    crate::native_surface::paged_line_fonts::NativeMarkdownLineFont::register(
                        selected,
                        &owner.catalog,
                        current,
                    )?;
                let selected = if owner.presentation_mode()
                    == omni_markdown_core::MarkdownPresentationMode::InlinePreview
                {
                    let metrics_owner = owner
                        .typography
                        .borrow()
                        .as_ref()
                        .expect("InlinePreview持唯一系统字体行标量owner")
                        .metrics_owner
                        .clone();
                    selected.capture_preview_metrics(&metrics_owner, current)?
                } else {
                    selected // Source cascade fonts preserve actual Core Text floating metrics without reading/creating preview line slots.
                };
                let selected = Rc::new(selected);
                owner.fonts.borrow_mut().push(selected.clone());
                selected
            }
        } else {
            base.clone()
        };
        ticket.require(owner, &self.source)?;
        let response = selected_font_response(
            owner,
            &layout_source,
            trial.request,
            trial.selection,
            &base,
            &font,
        )?;
        self.layout_composer(trial.target).provide_style(response)?;
        #[cfg(feature = "native-markdown-runtime-probe")]
        if let FontTrialTarget::Cell { row, column } = trial.target
            && actual_choice.0.len() > 1
            && actual_choice.0.end <= 8
            && actual_choice.2.glyph_count <= 8
            && std::env::var_os("OMNI_NATIVE_PROBE_TRACE").is_some()
        {
            // Diagnose complete multiunit graphemes near a short cell start only, using the
            // just-delivered original Choice/font registration.
            // Metadata/points are capture-certified pure values, without Native getters, text reads, or
            // new retained claims.
            eprintln!(
                "native_cell_font_choice row={row} column={column} whole_gcb={:?} origin={:?} glyph_facts={:?} points={} base_metadata={:?} selected_metadata={:?}",
                actual_choice.0,
                actual_choice.1,
                actual_choice.2,
                font.native().point_size(),
                base.native().core_font().metadata(),
                font.native().core_font().metadata(),
            );
        }
        if let Some(key) = trial.cache_key {
            // complete_trial_probed checked every glyph and returned the actual original Job/pages;
            // current Selection, Source, and response also passed, leaving weak font identity and a
            // finite key only in the cache.
            owner.font_choices.borrow_mut().remember(key, &font);
        }
        Ok(())
    }

    /// Consume actual ordered LineReady facts, checkpointing bounded distant batches before committing
    /// measured heights.
    fn accept_line(&mut self, owner: &Owner, facts: &MarkdownComposedLineFacts) -> Result<()> {
        if facts.kind == omni_markdown_core::MarkdownComposedLineKind::TableRow {
            let grid = self.grid.take().expect("真实TableRow来自当前已完成Grid");
            if !grid.submitted
                || grid.source_range() != facts.source_utf16
                || (grid.height() * 64.0).round() as i64 != facts.y_advance_26_6
            {
                return Err(Error::Unsupported);
            }
            self.grids.push_back(grid); // Retire by actual viewport on the next original turn; old row geometry remains sealed in the original output directory.
        }
        self.scan = None;
        self.top_lookup = None;
        if let Some(target) = self.target.as_mut() {
            target.reset_output();
        }
        if let Some(vertical) = self.vertical.as_mut() {
            vertical.reset_output();
        }
        // Batch actual new lines only while still before the target first scalar; ordinary
        // first-screen/scroll work publishes line by line,
        // and checkpoint the target line immediately even below 32 lines, without waiting for match
        // tail or whole paragraph.
        let distant = self.target.as_ref().is_some_and(|target| {
            target.request.pending()
                && !target.has_anchor()
                && facts.source_utf16.end < target.request.first_end
        });
        if self.queued_lines.is_some() || distant {
            let batch = self.queued_lines.get_or_insert_with(|| QueuedLines {
                first: self.lines,
                count: 0,
                last: facts.clone(),
            });
            if facts.ordinal
                != batch
                    .first
                    .checked_add(batch.count)
                    .ok_or(Error::CounterExhausted)?
                || batch.count == MAX_TARGET_BATCH_LINES
            {
                return Err(Error::Unsupported);
            }
            batch.count += 1;
            batch.last = facts.clone();
            self.target_max_batch_lines = self.target_max_batch_lines.max(batch.count);
            let reached = self
                .target
                .as_ref()
                .is_none_or(|target| facts.source_utf16.end >= target.request.first_end);
            if batch.count == MAX_TARGET_BATCH_LINES || reached {
                self.checkpoint_batch()?;
            }
            return Ok(()); // Each turn still executes one original Core quantum; finite batching reduces actual checkpoint frequency only.
        }
        // This slot keeps the last measured candidate only; return it before obtaining the same writer
        // next prefix,
        // rather than holding two Readers through intermediate candidate/old Scene and requesting a
        // third descriptor.
        let output = self.checkpoint_prefix(owner.source_target_pending())?;
        let handle = output.borrow_mut().last_line()?.ok_or(Error::Unsupported)?;
        if handle.facts() != facts || facts.ordinal != self.lines {
            return Err(Error::Unsupported);
        }
        self.pending_line = Some(PendingLine {
            output,
            metrics: LineMetrics::new(handle, owner.presentation_mode(), false),
            target_batch: false,
        });
        Ok(())
    }

    fn checkpoint_prefix(
        &mut self,
        target_work: bool,
    ) -> Result<Rc<RefCell<MarkdownComposedLines>>> {
        drop(self.output.take()); // Owner revoked the old business Scene; return the old Reader slot before syncing/duplicating the new prefix.
        let next_attempt = target_work
            .then(|| {
                self.target_checkpoint_attempts
                    .checked_add(1)
                    .ok_or(Error::CounterExhausted)
            })
            .transpose()?;
        let started = target_work.then(std::time::Instant::now);
        let result = self
            .composer
            .as_mut()
            .ok_or(Error::Closed)?
            .publish_ready_prefix();
        if let (Some(started), Some(attempts)) = (started, next_attempt) {
            // Measure the actual complete checkpoint entry including guards/sync/dup, without claiming
            // isolated fsync latency.
            self.target_checkpoint_attempts = attempts;
            self.target_checkpoint_elapsed += started.elapsed();
        }
        Ok(Rc::new(RefCell::new(result?.ok_or(Error::Unsupported)?)))
    }

    fn checkpoint_batch(&mut self) -> Result<()> {
        let batch = self.queued_lines.take().ok_or(Error::Closed)?;
        if batch.count == 0 || batch.count > MAX_TARGET_BATCH_LINES || batch.first != self.lines {
            return Err(Error::Unsupported);
        }
        // Each LineReady already advanced original Composer state; publication does not authorize
        // another step.
        // Certify existing LineEnds into the unique prefix Reader only, without fabricated lines or old
        // output identity reuse.
        let output = self.checkpoint_prefix(true)?;
        let last = output.borrow_mut().last_line()?.ok_or(Error::Unsupported)?;
        self.target_batch_index_records = self
            .target_batch_index_records
            .checked_add(2)
            .ok_or(Error::CounterExhausted)?;
        if last.facts() != &batch.last {
            return Err(Error::Unsupported);
        }
        self.pending_batch = Some(PendingBatch {
            output,
            first: batch.first,
            count: batch.count,
            cursor: Some(last),
            handles: VecDeque::with_capacity(batch.count),
            collecting: true,
        });
        Ok(())
    }

    fn advance_batch(&mut self, owner: &Owner) -> Result<()> {
        let mut batch = self.pending_batch.take().ok_or(Error::Closed)?;
        if batch.collecting {
            // previous_line performs four 64-byte reads; at most 31 predecessors follow the final line
            // already read by checkpoint,
            // with no part measurement in this turn, preserving bounded Native metadata work without
            // expanding an arbitrary prefix.
            for _ in 0..MAX_TARGET_BATCH_LINES {
                let handle = batch.cursor.take().ok_or(Error::Unsupported)?;
                let expected = batch.first + batch.count - batch.handles.len() - 1;
                if handle.facts().ordinal != expected {
                    return Err(Error::Unsupported);
                }
                let previous = if expected == batch.first {
                    None
                } else {
                    let previous = batch.output.borrow_mut().previous_line(&handle)?;
                    self.target_batch_index_records = self
                        .target_batch_index_records
                        .checked_add(4)
                        .ok_or(Error::CounterExhausted)?;
                    previous
                };
                batch.handles.push_front(handle);
                if expected == batch.first {
                    batch.collecting = false;
                    break;
                }
                batch.cursor = previous;
            }
            self.pending_batch = Some(batch);
            return Ok(()); // Weak-handle collection and actual font/part measurement use separate turns without stacking their allowances.
        }
        let mut parts = LINE_METRIC_BUDGET;
        // The original 32-part quota belongs to the complete measurement turn; short lines can consume
        // its remaining allowance.
        // At most 32 collected line handles also bound zero-part/control lines, without prefetching new
        // lines or another Reader.
        while let Some(handle) = batch.handles.pop_front() {
            if handle.facts().ordinal != self.lines {
                return Err(Error::Unsupported);
            }
            self.pending_line = Some(PendingLine {
                output: batch.output.clone(),
                metrics: LineMetrics::new(handle, owner.presentation_mode(), false),
                target_batch: true,
            });
            if !self.advance_line(owner, &mut parts)?
                || parts.metadata_units == 0
                || parts.records < 7
            {
                break; // Resume the same pending line/remaining batch next quantum when this line is unfinished or shared allowance is spent.
            }
        }
        if !batch.handles.is_empty() {
            self.pending_batch = Some(batch);
        }
        Ok(())
    }

    /// Single-line continuation and bounded distant batches share actual metric commit under the caller
    /// unique turn part allowance.
    fn advance_line(
        &mut self,
        owner: &Owner,
        parts: &mut MarkdownLineComposeBudget,
    ) -> Result<bool> {
        let mut line = self.pending_line.take().ok_or(Error::Closed)?;
        let before = line.metrics.part;
        let mut work = grid::Work::default();
        let ready = line.metrics.advance(&line.output, owner, parts, &mut work);
        self.record_grid_work(work)?;
        let ready = ready?;
        if line.target_batch {
            self.target_batch_index_records = self
                .target_batch_index_records
                .checked_add((line.metrics.part - before) * 7)
                .ok_or(Error::CounterExhausted)?;
            // Each actual part read includes two Line checks, two Part records, and three Directory
            // records: at most 224 records per 32-part turn.
        }
        if !ready {
            self.pending_line = Some(line);
            return Ok(false);
        }
        if line.metrics.handle.facts().ordinal != self.lines {
            return Err(Error::Unsupported); // Accumulated Y accepts the next actual original-order line only, rejecting missing or repeated measurement.
        }
        let old_height = self.height;
        self.height += line.metrics.height()?;
        if !self.height.is_finite() || self.height >= (1_u64 << 46) as f64 {
            return Err(ViewError::InvalidGeometry.into());
        }
        self.lines = self.lines.checked_add(1).ok_or(Error::CounterExhausted)?;
        if let Some(target) = self.target.as_mut() {
            target.observe_line(
                line.metrics.handle.facts(),
                old_height + self.padding,
                self.height + self.padding,
            );
        }
        self.output = Some(line.output);
        self.visual = None; // Replacing the sole prefix revokes the weak query; the queued permit retains its original head and side.
        self.scan = None;
        self.top_lookup = None;
        Ok(true)
    }

    fn publish(
        &mut self,
        owner: &Owner,
        request: &NativeMarkdownPagedSceneRequest,
        ticket: &PreparationTicket,
    ) -> Result<()> {
        if self.output.is_none() {
            return self.publish_grid(owner, request, ticket);
        }
        ticket.require(owner, &self.source)?;
        let view = owner.view()?;
        let viewport = owner.viewport.borrow().clone().ok_or(Error::Closed)?;
        let content_height = self.height();
        if viewport.geometry()?.content_width != self.maximum_content_width {
            owner.native_preparation_geometry(request, &viewport, || {
                viewport.set_content_width(self.maximum_content_width)
            })?;
            owner.schedule()?;
            return Ok(()); // Bounded pixels still consume the original viewport; this turn installs actual horizontal document extents only.
        }
        if viewport.geometry()?.content_height != content_height {
            owner.native_preparation_geometry(request, &viewport, || {
                viewport.set_content_height(content_height)
            })?;
            // The owned height transaction actually changes geometry. End this request and queue its
            // same continuation; a later turn
            // acquires the actual new request instead of capturing successor Input/width inside an old
            // callback to certify old Source.
            owner.schedule()?;
            return Ok(());
        }
        let needed_bottom = request.virtual_viewport.document_origin.y
            + request.viewport.size.height
            + self.padding;
        let output = self.output.as_ref().cloned().ok_or(Error::Unsupported)?;
        let top_prefix = self.top.as_ref().filter(|top| needed_bottom <= top.height);
        // A measured target line covering the actual viewport bottom can seed backward scanning without
        // restarting at distant EOF,
        // or including lines beyond required padding. The anchor remains bound to this
        // Source/font/width Producer.
        let target_anchor = self.last_target_anchor.as_ref().filter(|anchor| {
            anchor.bottom
                >= request.virtual_viewport.document_origin.y + request.viewport.size.height
        });
        let seed = match (top_prefix, target_anchor) {
            (Some(top), Some(anchor)) if top.last_ordinal < anchor.ordinal => {
                Some((top.last_ordinal, top.height))
            }
            (_, Some(anchor)) => Some((anchor.ordinal, anchor.bottom - self.padding)),
            (Some(top), None) => Some((top.last_ordinal, top.height)),
            (None, None) => None,
        };
        let prefix_height = seed.map_or(self.height, |(_, height)| height);
        if self
            .scan
            .as_ref()
            .is_some_and(|scan| !scan.request.same(request))
        {
            self.scan = None;
        }
        if self
            .top_lookup
            .as_ref()
            .is_some_and(|lookup| !lookup.request.same(request))
        {
            self.top_lookup = None; // Old lookup authority cannot certify a successor viewport after geometry changes; the original output remains unique.
        }
        let cursor = if self.scan.is_none() {
            if let Some((last_ordinal, _)) = seed {
                let lookup = match self.top_lookup.as_mut() {
                    Some(lookup) => lookup,
                    None => self.top_lookup.insert(TopLookup {
                        request: request.clone(),
                        cursor: output
                            .borrow_mut()
                            .line_lookup(MarkdownComposedLineLookupTarget::Ordinal(last_ordinal))?,
                    }),
                };
                let step = output
                    .borrow_mut()
                    .step_line_lookup(&mut lookup.cursor, METADATA_PER_TURN)?;
                match step.answer {
                    MarkdownComposedLineLookupAnswer::Pending => {
                        owner.schedule()?;
                        return Ok(()); // Resume the actual original file-index cursor next turn rather than rescanning from the last line.
                    }
                    MarkdownComposedLineLookupAnswer::Found(line) => {
                        if line.facts().ordinal != last_ordinal {
                            return Err(Error::Unsupported);
                        }
                        self.top_lookup = None;
                        Some(line)
                    }
                    MarkdownComposedLineLookupAnswer::PrefixNotCovered => {
                        return Err(Error::Unsupported); // The measured first screen must exist in the latest prefix; absence is a mismatch rather than empty Ready.
                    }
                }
            } else {
                self.top_lookup = None;
                output.borrow_mut().last_line()?
            }
        } else {
            None // The original scan already owns this candidate cursor; do not relocate or restart at the first-screen final line.
        };
        // Move this original scan to one local owner and end field borrowing before recording actual
        // read work.
        // Restore it intact for continuation; outer failure/close releases errors while preserving
        // reads already performed.
        let mut scan = match self.scan.take() {
            Some(scan) => scan,
            None => Scan {
                request: request.clone(),
                cursor,
                bottom: prefix_height + self.padding,
                first_top: self.padding,
                handles: VecDeque::new(),
                ranges: VecDeque::new(),
                paragraphs: VecDeque::new(),
                folded_ends: VecDeque::new(),
                simple_ltr: true,
                metrics: None,
                table_rows: VecDeque::new(),
            },
        };
        let top = request.virtual_viewport.document_origin.y;
        let bottom = top + request.viewport.size.height;
        let mut parts = LINE_METRIC_BUDGET;
        for _ in 0..METADATA_PER_TURN {
            if scan.metrics.is_none() {
                let Some(handle) = scan.cursor.take() else {
                    break;
                };
                scan.metrics = Some(LineMetrics::new(
                    handle,
                    owner.presentation_mode(),
                    request.target.cell_alignment().is_some_and(|alignment| {
                        alignment != omni_markdown_core::MarkdownTableAlignment::Left
                    }),
                ));
            }
            let mut metrics = scan.metrics.take().ok_or(Error::Closed)?;
            let mut work = grid::Work::default();
            let ready = metrics.advance(&output, owner, &mut parts, &mut work);
            self.record_grid_work(work)?;
            if !ready? {
                scan.metrics = Some(metrics);
                self.scan = Some(scan);
                owner.schedule()?;
                return Ok(());
            }
            let height = metrics.height()?;
            let ltr = metrics.ltr;
            let folded_end = metrics.folded_end;
            let handle = metrics.into_measured()?;
            // A growing prefix below the viewport cannot relocate the same visible line through a
            // different backward-subtraction chain.
            // Reanchor measured bounds only on the actual target line with complete original request
            // authority, never a bare historical anchor.
            let anchor = self.target.as_ref().and_then(|target| {
                target
                    .current_line_anchor(owner, request, self.source.authority())
                    .filter(|anchor| anchor.ordinal == handle.facts().ordinal)
                    .cloned()
            });
            let line_top = if let Some(anchor) = anchor {
                scan.reanchor_table_rows(&anchor)?;
                scan.bottom = anchor.bottom;
                anchor.top
            } else {
                scan.bottom - height
            };
            let previous = output.borrow_mut().previous_line(handle.handle())?;
            if scan.bottom >= top && line_top <= bottom {
                if scan.handles.len() == MAX_WINDOW_LINES {
                    return Err(Error::Unsupported);
                }
                scan.first_top = line_top;
                scan.simple_ltr &= ltr;
                scan.ranges.push_front(handle.facts().source_utf16.clone());
                scan.folded_ends.push_front(folded_end);
                scan.paragraphs
                    .push_front(handle.facts().paragraph_utf16.clone());
                if handle.facts().kind == omni_markdown_core::MarkdownComposedLineKind::TableRow {
                    // Actual row tops come from the same directory scan and remain in this bounded
                    // window, without historical Native row indices.
                    scan.table_rows
                        .push_front((handle.facts().ordinal, line_top));
                }
                scan.handles.push_front(ScannedLine { handle, height });
            }
            scan.bottom = line_top;
            // After covering the actual viewport top, earlier lines cannot enter this bounded window.
            if line_top <= top {
                scan.cursor = None;
                break;
            }
            scan.cursor = previous;
        }
        if scan.cursor.is_some() || scan.metrics.is_some() {
            self.scan = Some(scan);
            owner.schedule()?;
            return Ok(());
        }
        if scan.handles.is_empty() {
            return self.publish_grid(owner, request, ticket); // Body text lies above; the current table prefix fabricates no text line.
        }
        if self.begin_missing_grid_restoration(&output, &scan)? {
            self.scan = Some(scan); // Preserve this original window and restore missing cells only, without restarting at the body end.
            owner.schedule()?;
            return Ok(());
        }
        let geometry = viewport.geometry()?;
        let nonce = NativeMarkdownPagedSceneNonce::new();
        let key = NativeMarkdownPagedSceneKey {
            presentation_mode: owner.presentation_mode(),
            request: request.clone(),
            source: self.source.authority(),
            nonce: nonce.authority(),
        };
        let source = self.source.clone();
        let current = || ticket.current(owner, &source) && geometry.is_current();
        let Some(grids) = self.prepare_grids(owner, &key, &current)? else {
            self.scan = Some(scan); // The complete body window keeps original weak handles while cell directories continue, without body-last-line rescanning.
            owner.schedule()?;
            return Ok(());
        };
        let palette = owner.fonts.borrow().clone();
        let first_ordinal = scan
            .handles
            .front()
            .ok_or(Error::Unsupported)?
            .handle
            .facts()
            .ordinal;
        let first_top = NSPoint::new(
            request.virtual_viewport.layout_origin_x + self.padding
                - request.virtual_viewport.document_origin.x,
            request.viewport.size.height + top - scan.first_top,
        );
        let window = Rc::new(
            NativeMarkdownComposedViewport::prepare(
                output.clone(),
                key.clone(),
                palette,
                scan.handles.into_iter().map(|measured| measured.handle),
                first_top,
                &|| ticket.current(owner, &self.source) && geometry.is_current(),
            )
            .map_err(scene_error)?,
        );
        ticket.require(owner, &self.source)?;
        let (head_affinity, selections, active_line) = {
            let current = |epoch| {
                owner.current(epoch)
                    && ticket.current(owner, &self.source)
                    && owner
                        .input
                        .state()
                        .is_ok_and(|actual| actual == request.input)
                    && geometry.is_current()
            };
            let permit = owner
                .input
                .capture(owner.configuration.epoch, &current)
                .map_err(ViewError::Input)?;
            let (upstream, _) = owner
                .input
                .navigation_selection_facts(&permit, &current)
                .map_err(ViewError::Input)?;
            // Use the same original caret-affinity ticket for at most 1000 integer ranges, without
            // selected-text reads or Source recapture.
            let selections = owner
                .input
                .selected_ranges(&permit, &current)
                .map_err(ViewError::Input)?
                .into_boxed_slice();
            let active_line = if owner.presentation_mode()
                == omni_markdown_core::MarkdownPresentationMode::Source
                && request.target.is_body(&request.document)
                && request.input.selection_range_count == 1
            {
                match request.input.directed_selection() {
                    Some(selection) => {
                        // The original ticket fixes document/generation/complete Input; pure Core
                        // borrows cross no Native call, and Bidi segments cannot replace logical lines.
                        let document = owner
                            .document
                            .borrow()
                            .as_ref()
                            .expect("Source Body持有原canonical文档")
                            .clone();
                        let range =
                            source_line_range_at_head(&document.borrow(), selection.head_utf16)
                                .map_err(ViewError::Document)?;
                        Some(range)
                    }
                    None => None, // Without certified direction, do not infer head from an ordered range or consume an old memento.
                }
            } else {
                None // Cell, Preview, and multiselection lack this Source single-directed-active-line authority.
            };
            (
                upstream.map(crate::native_surface::paged_text_view::NativeMarkdownPagedHeadAffinity::from_upstream),
                selections,
                active_line,
            )
        };
        let gutter = if owner.gutter.borrow().is_some() {
            let current = |epoch| owner.current(epoch) && ticket.current(owner, &self.source);
            let permit = owner
                .input
                .capture(owner.configuration.epoch, &current)
                .map_err(ViewError::Input)?;
            let folded_headers = owner
                .input
                .folded_header_lines(&permit, &current)
                .map_err(ViewError::Input)?;
            let document = owner
                .document
                .borrow()
                .as_ref()
                .expect("Source行号持有原canonical文档")
                .clone();
            let document = document.borrow();
            let mut rows = Vec::with_capacity(scan.ranges.len());
            let mut previous: Option<(usize, std::ops::Range<usize>)> = None;
            for (index, range) in scan.ranges.iter().enumerate() {
                let logical = match previous.as_ref() {
                    Some((number, logical))
                        if logical.start <= range.start
                            && (range.start < logical.end
                                || logical.is_empty() && range.start == logical.start) =>
                    {
                        (*number, logical.clone()) // Later soft wraps of one long logical line reuse these local facts without another tree query.
                    }
                    _ => {
                        source_line_at_head(&document, range.start).map_err(ViewError::Document)?
                    }
                };
                rows.push(crate::native_surface::gutter::paged::GutterRow {
                    logical_line: logical.0,
                    number: (range.start == logical.1.start).then(|| logical.0 + 1),
                    folded: folded_headers.contains(&logical.0),
                    may_fold: range.start == logical.1.start
                        && omni_markdown_core::markdown_folding::markdown_line_may_fold(
                            &document, logical.0,
                        )
                        .map_err(ViewError::Document)?,
                    frame: window.line_frame(index).ok_or(ViewError::InvalidGeometry)?,
                    active: active_line
                        .as_ref()
                        .is_some_and(|active| *active == logical.1),
                });
                previous = Some(logical);
            }
            Some(rows.into_boxed_slice())
        } else {
            None // Cell and Preview mount no ruler and read no canonical logical lines for line numbering.
        };
        ticket.require(owner, &self.source)?; // End all document/Rc slot borrows before rechecking original Source and same-attempt Native authority for publication.
        let ranges = scan
            .ranges
            .into_iter()
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let Some(mut highlights) =
            owner.prepare_find_highlights(&key, Some(&window), &grids, || {
                ticket.require(owner, &self.source)
            })?
        else {
            owner.schedule()?;
            return Ok(()); // Query getter reentry changed intent; the old candidate cannot publish empty/old matches for a successor query.
        };
        let find_highlights = std::mem::take(&mut highlights.ranges);
        let scene = Rc::new(Scene::new(super::scene::Publication {
            key,
            nonce,
            geometry,
            window: Some(window),
            output: Some(output),
            grids,
            owner: owner.weak.clone(),
            ranges,
            paragraphs: scan
                .paragraphs
                .into_iter()
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            folded_ends: scan
                .folded_ends
                .into_iter()
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            simple_ltr: scan.simple_ltr,
            width_key: self.width,
            layout_epoch: owner.layout_epoch.get(),
            colors: owner.configuration.colors,
            head_affinity,
            first_ordinal,
            selections,
            find_highlights,
            active_line,
            gutter,
        }));
        drop(owner.scene.replace(Some(scene.clone())));
        drop(highlights); // Return temporary bar/controller Native claims after installation so a reentrant new query can immediately revoke the original Scene.
        let gutter = owner.gutter.borrow().clone();
        if let Some(gutter) = gutter {
            gutter.publish(&scene)?;
        }
        view.scene_ready()?;
        ticket.require(owner, &self.source)?;
        owner.mode_scene_published();
        if self.top.is_none() && top == 0.0 {
            self.top = Some(TopPrefix {
                last_ordinal: self.lines.checked_sub(1).ok_or(Error::Unsupported)?,
                height: self.height,
            });
        }
        Ok(())
    }

    /// Restore one actual sealed row missing from the current window, without remeasuring existing
    /// widths or whole-table Native indices.
    fn begin_missing_grid_restoration(
        &mut self,
        output: &Rc<RefCell<MarkdownComposedLines>>,
        scan: &Scan,
    ) -> Result<bool> {
        for (ordinal, top) in &scan.table_rows {
            let handle = scan
                .handles
                .iter()
                .map(|line| &line.handle)
                .find(|line| line.facts().ordinal == *ordinal)
                .expect("表格行顶只来自同一窗口真实 handle");
            let height = handle.facts().y_advance_26_6 as f64 / 64.0;
            if *top
                >= scan.request.virtual_viewport.document_origin.y
                    + scan.request.viewport.size.height
                || *top + height <= scan.request.virtual_viewport.document_origin.y
            {
                continue; // Rows touching only a directory boundary have no visible intersection; do not restore and immediately retire them repeatedly.
            }
            if self
                .grids
                .iter()
                .chain(self.grid.iter())
                .any(|grid| grid.source_range() == handle.facts().source_utf16)
            {
                continue;
            }
            // A missing old row creates one actual lookup continuation; finish it before the next
            // visible row instead of whole-table preparation.
            self.restoring_grid = Some(GridRestoration {
                output: output.clone(),
                lookup: Some(
                    output
                        .borrow_mut()
                        .line_lookup(MarkdownComposedLineLookupTarget::Ordinal(*ordinal))?,
                ),
                line: None,
                top: *top,
                height,
                grid: None,
            });
            return Ok(true);
        }
        Ok(false)
    }

    /// Share one actual scan allowance across rows and assemble current visible-cell windows only after
    /// all scans finish.
    fn prepare_grids(
        &mut self,
        owner: &Owner,
        key: &NativeMarkdownPagedSceneKey,
        current: &dyn Fn() -> bool,
    ) -> Result<Option<Box<[super::scene::GridRowPublication]>>> {
        let mut budget = LINE_METRIC_BUDGET;
        let mut work = grid::Work::default();
        let scanned = (|| -> Result<bool> {
            for grid in self.grids.iter_mut().chain(self.grid.iter_mut()) {
                if grid.visible(&key.request)
                    && !grid.scan_windows(owner, &key.request, &mut budget, &mut work)?
                {
                    return Ok(false);
                }
            }
            Ok(true)
        })();
        self.record_grid_work(work)?;
        if !scanned? {
            return Ok(None);
        }
        // Create no Native cell window before all scans finish, preventing repeated early-cell
        // allocation from starving later cells.
        let mut rows = Vec::new();
        for grid in self.grids.iter_mut().chain(self.grid.iter_mut()) {
            if grid.visible(&key.request) {
                rows.push(grid.prepare(owner, key, current)?);
            }
        }
        Ok(Some(rows.into_boxed_slice()))
    }

    /// Before first-row Core LineReady, publish actual cell prefixes only, without fabricated empty
    /// body windows or EOF.
    fn publish_grid(
        &mut self,
        owner: &Owner,
        request: &NativeMarkdownPagedSceneRequest,
        ticket: &PreparationTicket,
    ) -> Result<()> {
        ticket.require(owner, &self.source)?;
        let viewport = owner.viewport.borrow().clone().ok_or(Error::Closed)?;
        if viewport.geometry()?.content_width != self.maximum_content_width {
            owner.native_preparation_geometry(request, &viewport, || {
                viewport.set_content_width(self.maximum_content_width)
            })?;
            owner.schedule()?;
            return Ok(()); // Solved column widths can publish before full-row sealing, without renewing old Scene geometry.
        }
        if viewport.geometry()?.content_height != self.height() {
            // Publish measured actual content extents first; geometry changes require a new Scene key
            // rather than old pixel-coordinate certification.
            owner.native_preparation_geometry(request, &viewport, || {
                viewport.set_content_height(self.height())
            })?;
            owner.schedule()?;
            return Ok(());
        }
        let geometry = viewport.geometry()?;
        let nonce = NativeMarkdownPagedSceneNonce::new();
        let key = NativeMarkdownPagedSceneKey {
            presentation_mode: owner.presentation_mode(),
            request: request.clone(),
            source: self.source.authority(),
            nonce: nonce.authority(),
        };
        let source = self.source.clone();
        let current = || ticket.current(owner, &source) && geometry.is_current();
        let Some(grids) = self.prepare_grids(owner, &key, &current)? else {
            // The same Scene request awaits its bounded scan continuation; remain Pending and queue the
            // next turn.
            owner.schedule()?;
            return Ok(());
        };
        let input_current = |epoch| owner.current(epoch) && current();
        // Selection uses original current Input authority; grid-only publication creates no new Input
        // graph or assumed empty selection.
        let permit = owner
            .input
            .capture(owner.configuration.epoch, &input_current)
            .map_err(ViewError::Input)?;
        let (upstream, _) = owner
            .input
            .navigation_selection_facts(&permit, &input_current)
            .map_err(ViewError::Input)?;
        let selections = owner
            .input
            .selected_ranges(&permit, &input_current)
            .map_err(ViewError::Input)?
            .into_boxed_slice();
        ticket.require(owner, &self.source)?;
        // No ordinary body window means actual None; paint real cell prefixes without an invented empty
        // TextWindow.
        let Some(mut highlights) = owner
            .prepare_find_highlights(&key, None, &grids, || ticket.require(owner, &self.source))?
        else {
            owner.schedule()?;
            return Ok(());
        };
        let find_highlights = std::mem::take(&mut highlights.ranges);
        let scene = Rc::new(Scene::new(super::scene::Publication {
            key, nonce, geometry, window: None, output: None, grids, owner: owner.weak.clone(),
            ranges: Box::new([]), paragraphs: Box::new([]), folded_ends: Box::new([]), simple_ltr: false,
            width_key: self.width, layout_epoch: owner.layout_epoch.get(), colors: owner.configuration.colors,
            head_affinity: upstream.map(crate::native_surface::paged_text_view::NativeMarkdownPagedHeadAffinity::from_upstream),
            first_ordinal: self.lines, selections, find_highlights, active_line: None, gutter: None,
        }));
        drop(owner.scene.replace(Some(scene)));
        drop(highlights);
        owner.view()?.scene_ready()?;
        ticket
            .require(owner, &self.source)
            .inspect(|()| owner.mode_scene_published())
    }
}

/// Locate the original active head through canonical line-tree aggregates and at most a 4 KiB hit leaf,
/// without paragraph scanning.
pub(super) fn source_line_range_at_head(
    document: &omni_markdown_core::MarkdownDocumentBuffer,
    head: usize,
) -> std::result::Result<std::ops::Range<usize>, omni_markdown_core::MarkdownDocumentBufferError> {
    source_line_at_head(document, head).map(|(_, range)| range)
}

/// Line numbering and active background share CRLF/trailing-empty-line semantics, returning tree
/// ordinals/endpoints without logical-line copies.
fn source_line_at_head(
    document: &omni_markdown_core::MarkdownDocumentBuffer,
    head: usize,
) -> std::result::Result<
    (usize, std::ops::Range<usize>),
    omni_markdown_core::MarkdownDocumentBufferError,
> {
    let byte = document.byte_offset_for_utf16(head)?;
    let line = document.line_for_byte(byte)?;
    let start = document.byte_offset_for_line(line)?;
    let (line, start) = if start > byte {
        // The prefix counted CR before actual next-line start after CRLF; LF interior still belongs to
        // the previous line without new reads or head changes.
        (line - 1, document.byte_offset_for_line(line - 1)?)
    } else {
        (line, start) // Ordinary scalar boundaries use actual line-tree results, without Bidi or soft-wrap seams becoming logical lines.
    };
    let end = if line + 1 < document.line_count() {
        document.byte_offset_for_line(line + 1)?
    } else {
        document.byte_len() // Actual EOF has no next line; an original trailing newline yields start==end for the final empty line.
    };
    Ok((
        line,
        document.utf16_offset_for_byte(start)?..document.utf16_offset_for_byte(end)?,
    ))
}

impl Owner {
    /// Consume the delivered original worker result once; stale or unaccepted results still retire
    /// through fallible resource cleanup.
    pub(super) fn finish_font_trial(&self, delivered_serial: u64) -> Result<()> {
        let (serial, result) = self
            .worker
            .borrow()
            .as_ref()
            .ok_or(Error::Closed)?
            .take_delivered()?;
        if serial != delivered_serial {
            return merge_preparation_cleanup(
                Err(Error::WorkerFailed),
                result.retire(false).map_err(Error::from),
            );
        }
        if !self.active.get() || self.serial.get() != serial {
            return result.retire(false).map_err(Error::from); // Late delivery loses font-choice authority only; actual resource failures still belong to the original Body.
        }
        let mut delivered = Some(result);
        let admission = (|| {
            let mut producer = self.producer.try_borrow_mut().map_err(|_| Error::Busy)?;
            let trial = producer
                .as_mut()
                .and_then(|producer| producer.trial.as_mut())
                .ok_or(Error::Closed)?;
            if !trial.in_flight {
                return Err(Error::WorkerFailed);
            }
            trial.in_flight = false;
            trial.result = delivered.take();
            Ok(())
        })();
        if let Err(cause) = admission {
            // End the slot borrow before retiring unaccepted results here, preserving actual I/O
            // failure despite state rejection.
            let cleanup = delivered
                .expect("未接纳分支仍持本次唯一结果")
                .retire(false)
                .map_err(Error::from);
            return merge_preparation_cleanup(Err(cause), cleanup);
        }
        self.schedule()
    }
}

impl Drop for Producer {
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Some(target) = self.target.take() {
            target.request.revoke();
        }
        // Return bounded unmeasured batches with the actual Reader; cancellation cannot clear a target
        // flag while leaving a batch in a successor session.
        self.queued_lines = None;
        drop(self.pending_line.take());
        drop(self.pending_batch.take());
        let trial = self.trial.take();
        if let Some(mut trial) = trial {
            // Fallback Drop requests the same in-flight ShapeCancellation; Body owns the actual worker
            // and disconnects/joins at close, without mistaking cancellation for actual ShapeJob
            // resource return.
            drop(trial.fallback);
            drop(trial.result.take());
        }
        self.pending_style = None;
        self.pending_control = None;
        drop(self.semantics.take()); // Actually cancel the unique Source request and return Ready without relying on stale Native work for cleanup.
        drop(self.composer.take());
        self.vertical = None;
        drop(self.output.take());
    }
}

pub(super) fn width(request: &NativeMarkdownPagedSceneRequest, padding: f64) -> Result<i64> {
    let points = request.virtual_viewport.layout_width - padding * 2.0;
    let fixed = points * 64.0;
    if !fixed.is_finite() || fixed <= 0.0 || fixed >= i64::MAX as f64 {
        return Err(ViewError::InvalidGeometry.into());
    }
    Ok(fixed.round() as i64)
}

impl LineMetrics {
    fn new(
        handle: MarkdownComposedLine,
        mode: omni_markdown_core::MarkdownPresentationMode,
        measure_alignment: bool,
    ) -> Self {
        let visible_end = handle.facts().source_utf16.start;
        Self {
            handle,
            part: 0,
            geometry: crate::native_surface::preview_paragraph::PreviewLineMetrics::new(mode),
            ltr: true,
            visible_end,
            folded_end: None,
            measure_alignment,
            alignment: None,
        }
    }
    fn advance(
        &mut self,
        output: &RefCell<MarkdownComposedLines>,
        owner: &Owner,
        budget: &mut MarkdownLineComposeBudget,
        work: &mut grid::Work,
    ) -> Result<bool> {
        if self.measure_alignment {
            if self.alignment.is_none() {
                self.alignment = Some(
                    NativeMarkdownLineAlignmentMeasure::new(output, &self.handle)
                        .map_err(scene_error)?,
                );
            }
            let alignment = self.alignment.as_mut().expect("原对齐查询已创建");
            if !alignment.source_ready() {
                if budget.bytes == 0
                    || budget.scalars == 0
                    || budget.records == 0
                    || budget.metadata_units == 0
                {
                    return Ok(false);
                }
                let step = alignment
                    .step_source(output, &self.handle, *budget)
                    .map_err(scene_error)?;
                budget.bytes -= step.bytes;
                budget.scalars -= step.scalars;
                budget.records -= step.records;
                budget.metadata_units -= step.metadata_units;
                work.alignment_step(&step)?;
                if !alignment.source_ready() {
                    return Ok(false); // Retain the original trailing cursor without treating unread whitespace/text as completed width.
                }
            }
        }
        let palette = owner.fonts.borrow().clone();
        while self.part < self.handle.facts().parts
            && budget.metadata_units != 0
            && budget.records >= 7
        {
            let part = output
                .borrow_mut()
                .part(&self.handle, self.part)?
                .ok_or(Error::Unsupported)?;
            budget.metadata_units -= 1;
            budget.records -= 7; // Charge the original part transaction: two Line checks, two Part records, and three actual Directory records.
            match part.facts().kind {
                omni_markdown_core::MarkdownComposedLinePartKind::Glyphs
                | omni_markdown_core::MarkdownComposedLinePartKind::Tab => {
                    self.visible_end = part.facts().source_utf16.end;
                }
                omni_markdown_core::MarkdownComposedLinePartKind::FoldedSection => {
                    // Original visible glyph/Tab end plus actual opaque control certifies the header
                    // line end;
                    // CR/LF and hidden lines inside folds cannot become EndOfLine or in-line deletion
                    // targets.
                    self.folded_end = Some(self.visible_end);
                }
                omni_markdown_core::MarkdownComposedLinePartKind::ParagraphBreak => {}
                omni_markdown_core::MarkdownComposedLinePartKind::TableRow => {
                    self.part += 1;
                    continue; // The table block owns actual complete-cell height; its directory font cannot create an ordinary text line slot.
                }
            }
            let authority = part.font_registration().authority()?;
            let font = palette
                .iter()
                .find(|font| font.native().core_font().permits(&authority))
                .ok_or(Error::Unsupported)?;
            font.require(part.font_registration())?;
            let metrics = font.metrics()?;
            self.geometry
                .add(
                    part.facts(),
                    metrics,
                    (owner.presentation_mode()
                        == omni_markdown_core::MarkdownPresentationMode::InlinePreview)
                        .then(|| font.preview_metrics()),
                )
                .ok_or(ViewError::InvalidGeometry)?;
            self.ltr &= part.facts().level == 0;
            if let Some(alignment) = self.alignment.as_mut() {
                let input = owner.input.state().map_err(ViewError::Input)?;
                let serial = owner.serial.get();
                alignment
                    .observe_part(output, &part, font, &|| {
                        owner.current_body()
                            && owner.serial.get() == serial
                            && owner.input.state().is_ok_and(|actual| actual == input)
                    })
                    .map_err(scene_error)?;
            }
            self.part += 1;
        }
        Ok(self.part == self.handle.facts().parts)
    }

    // Only complete original part/alignment measurement may issue a Native line handle for painting.
    fn into_measured(self) -> Result<NativeMarkdownMeasuredLine> {
        if self.part != self.handle.facts().parts {
            return Err(ViewError::InvalidGeometry.into());
        }
        match self.alignment {
            Some(alignment) => Ok(alignment.finish(self.handle).map_err(scene_error)?),
            None if !self.measure_alignment => Ok(self.handle.into()),
            None => Err(ViewError::Revoked.into()),
        }
    }

    fn aligned_origin_x(
        &self,
        alignment: omni_markdown_core::MarkdownTableAlignment,
        width: f64,
    ) -> Result<f64> {
        if alignment == omni_markdown_core::MarkdownTableAlignment::Left {
            return Ok(self.geometry.paragraph.inset);
        }
        Ok(self
            .alignment
            .as_ref()
            .ok_or(ViewError::InvalidGeometry)?
            .origin_x(alignment, width, self.geometry.paragraph.inset)
            .map_err(scene_error)?)
    }
    fn height(&self) -> Result<f64> {
        if self.part != self.handle.facts().parts {
            return Err(ViewError::InvalidGeometry.into());
        }
        if self.handle.facts().kind == omni_markdown_core::MarkdownComposedLineKind::TableRow {
            let height = self.handle.facts().y_advance_26_6 as f64 / 64.0;
            return if height.is_finite() && height > 0.0 {
                Ok(height)
            } else {
                Err(ViewError::InvalidGeometry.into())
            };
        }
        self.geometry
            .geometry(self.handle.facts())
            .map(|geometry| geometry.advance)
            .ok_or_else(|| ViewError::InvalidGeometry.into())
    }
}

pub(super) fn scene_error(
    cause: crate::native_surface::paged_line_viewport::NativeMarkdownComposedViewportError,
) -> ViewError {
    use crate::native_surface::paged_line_viewport::NativeMarkdownComposedViewportError as Cause;
    match cause {
        Cause::Composed(omni_markdown_core::MarkdownLineComposeError::Source(cause))
        | Cause::Geometry(
            crate::native_surface::paged_line_geometry::NativeMarkdownLineGeometryError::Composed(
                omni_markdown_core::MarkdownLineComposeError::Source(cause),
            ),
        ) => ViewError::Document(cause),
        Cause::Table(
            omni_markdown_core::markdown_document_buffer::MarkdownTableRequestError::Source(cause),
        ) => ViewError::Document(cause),
        Cause::Revoked => ViewError::Revoked,
        Cause::Busy => ViewError::Busy,
        // Offscreen/missing geometry grants no automatic retry; Pending requires actual work separately
        // queued by the owner.
        Cause::Pending => ViewError::InvalidRange,
        Cause::InvalidRange => ViewError::InvalidRange,
        other => ViewError::Raster(other.to_string()),
    }
}

#[cfg(test)]
mod cleanup_tests {
    use super::*;
    use omni_markdown_core::{
        MarkdownDocumentBufferError as Document, MarkdownLineComposeError as Compose,
        MarkdownShapingIoOperation,
    };

    #[test]
    /// A later cleanup Source error cannot replace the original Source failure inside a style error;
    /// preserve both errnos.
    fn style_reset_preserves_original_source_error_priority_during_cleanup() {
        let primary = Error::Compose(Compose::StyleResetCleanup {
            cause: Box::new(Compose::Source(Document::EditStorageUnavailable(libc::EIO))),
            operation: MarkdownShapingIoOperation::Unmap,
            errno: libc::EBUSY,
        });
        let cleanup = Error::View(ViewError::Document(Document::OriginalBackingUnavailable(
            libc::EBADF,
        )));
        let Error::PreparationCleanup { cause, cleanup } =
            merge_preparation_cleanup(Err(primary), Err(cleanup)).unwrap_err()
        else {
            panic!("两次实际错误类型不能退化为单一通用失败");
        };
        // Check existing typed-error propagation only, without claiming actual kernel EIO or munmap
        // fault injection.
        let Error::Compose(Compose::StyleResetCleanup {
            cause,
            operation,
            errno,
        }) = *cause
        else {
            panic!("原样式错误被后来的清理错误抢占");
        };
        assert!(matches!(
            *cause,
            Compose::Source(Document::EditStorageUnavailable(libc::EIO))
        ));
        assert_eq!(operation, MarkdownShapingIoOperation::Unmap);
        assert_eq!(errno, libc::EBUSY);
        assert!(matches!(
            *cleanup,
            Error::View(ViewError::Document(Document::OriginalBackingUnavailable(
                libc::EBADF
            )))
        ));
    }
}

#[cfg(test)]
mod active_line_tests {
    use std::io::Write;

    use omni_markdown_core::{MarkdownDocumentBuffer, MarkdownTextSelection};

    use super::source_line_range_at_head;

    /// Retain actual files through assertions; active-line tests use the production canonical mmap line
    /// tree.
    fn document(content: &str) -> (tempfile::NamedTempFile, MarkdownDocumentBuffer) {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(content.as_bytes()).unwrap();
        let document = MarkdownDocumentBuffer::open(file.path(), 4 * 1024 * 1024).unwrap();
        (file, document)
    }

    #[test]
    /// Original Source logical lines do not change with Bidi boundaries, emoji width, or viewport soft
    /// wraps.
    fn logical_line_uses_crlf_not_uba_paragraph_or_soft_wrap_boundaries() {
        // NEL, LS, and PS can affect Bidi or glyph wrapping without becoming original Source CR/LF
        // logical breaks.
        let (_file, document) = document("甲😀\r\nab\rcd\nef\u{85}gh\u{2028}ij\u{2029}kl");
        for (head, expected) in [
            (0, 0..5),
            (1, 0..5),
            (3, 0..5),
            (4, 0..5), // CRLF-interior observation changes line ownership only, without changing actual head or original line-ending bytes.
            (5, 5..8),
            (7, 5..8),
            (8, 8..11),
            (10, 8..11),
            (11, 11..22),
            (14, 11..22),
            (17, 11..22),
            (20, 11..22),
            (22, 11..22),
        ] {
            assert_eq!(source_line_range_at_head(&document, head), Ok(expected));
        }
    }

    #[test]
    /// Reverse multiline selection, actual trailing empty lines, and post-undo publications all use the
    /// canonical active head.
    fn active_head_keeps_nonempty_reverse_selection_and_true_eof_line() {
        let (_file, mut document) = document("first\nsecond\n");
        // Locate reverse multiline selection by its actual head, without using ordered-range end or
        // highlighting empty selections only.
        let reverse = MarkdownTextSelection {
            anchor_utf16: 11,
            head_utf16: 2,
        };
        document.set_selection(reverse).unwrap();
        assert_eq!(
            source_line_range_at_head(&document, document.selection().head_utf16),
            Ok(0..6)
        );
        assert_eq!(document.selection(), reverse);
        assert_eq!(source_line_range_at_head(&document, 12), Ok(6..13));
        assert_eq!(source_line_range_at_head(&document, 13), Ok(13..13));
        // After actual editing query the new root immediately; undo to the original root must preserve
        // its trailing empty line.
        document
            .replace_utf16_range(
                6..12,
                "中\r\n文",
                MarkdownTextSelection {
                    anchor_utf16: 10,
                    head_utf16: 10,
                },
            )
            .unwrap();
        assert_eq!(source_line_range_at_head(&document, 9), Ok(9..11));
        document.undo().unwrap();
        assert_eq!(source_line_range_at_head(&document, 13), Ok(13..13));
    }

    #[test]
    /// Huge original single lines return their full logical range, while empty Source grants only its
    /// actual EOF empty line without materialized text.
    fn giant_single_line_and_empty_document_need_no_materialized_text() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let block = [b'a'; 4096];
        // The actual two-MiB source contains one logical line; write test input in chunks without a
        // whole-paragraph test String.
        for _ in 0..512 {
            file.write_all(&block).unwrap();
        }
        let length = block.len() * 512;
        let document = MarkdownDocumentBuffer::open(file.path(), 4 * 1024 * 1024).unwrap();
        for head in [0, 4095, 4096, length / 2, length] {
            assert_eq!(source_line_range_at_head(&document, head), Ok(0..length));
        }
        let (_empty_file, empty) = self::document("");
        assert_eq!(source_line_range_at_head(&empty, 0), Ok(0..0));
    }
}
