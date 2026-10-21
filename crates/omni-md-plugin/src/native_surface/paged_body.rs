//! Production paginated body owner using the same canonical document and bounded viewport pipeline.
//!
//! Body and table-Cell preparation share the same source-aware pipeline, without TextKit, a
//! whole-document string, or another document.
//! A local frame or Source integration alone does not establish whole-product feature or memory
//! acceptance.

use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::{Rc, Weak},
};

use dispatch2::MainThreadBound;
use objc2::{MainThreadOnly, rc::Retained};
use objc2_app_kit::{NSParagraphStyle, NSScrollView};
use objc2_core_foundation::{CFRetained, Type};
use objc2_core_text::CTFont;
use objc2_foundation::MainThreadMarker;
use omni_markdown_core::{MarkdownDocumentBuffer, MarkdownLineFontCatalog};

use super::{
    paged_fonts::NativeMarkdownPagedFont,
    paged_input::{
        NativeMarkdownPagedCellSession, NativeMarkdownPagedInput, NativeMarkdownPagedInputCommit,
    },
    paged_line_fonts::NativeMarkdownLineFont,
    paged_text_view::{
        NativeMarkdownPagedCloseFailure, NativeMarkdownPagedCloseReceipt,
        NativeMarkdownPagedRenderStrategy, NativeMarkdownPagedSceneKey,
        NativeMarkdownPagedSceneRequest, NativeMarkdownPagedTextScene, NativeMarkdownPagedTextView,
        NativeMarkdownPagedTextViewDelegate, NativeMarkdownPagedTextViewError as ViewError,
        NativeMarkdownPagedVirtualViewport,
    },
    paged_viewport::NativeMarkdownPagedViewport,
};

mod cell;
mod clipboard;
mod find;
mod find_highlights;
mod font_choice;
mod link;
mod mode;
mod navigation;
mod notifications;
mod page_scroll;
mod paint_observer;
mod preparation;
#[cfg(feature = "native-markdown-runtime-probe")]
mod preparation_probe;
mod presentation;
mod producer;
pub(super) mod scene;
mod semantics;
mod target;
mod word;
pub use paint_observer::NativeMarkdownPagedBodyPaintObserver;
use paint_observer::PaintObservation;
#[cfg(feature = "native-markdown-runtime-probe")]
pub use preparation_probe::{
    NativeMarkdownPagedBodyPreparationProbe, NativeMarkdownPagedBodyPreparationResources,
};
use producer::{FontWorker, Producer};
use scene::Scene;
pub use target::NativeMarkdownPagedBodySourceTarget;

#[derive(Debug, thiserror::Error)]
pub enum NativeMarkdownPagedBodyError {
    #[error("原正文已经封闭")]
    Closed,
    #[error("原正文仍在同步输入或准备阶段")]
    Busy,
    #[error("原正文计数耗尽")]
    CounterExhausted,
    #[error("当前分页候选尚未实现这个正文语义")]
    Unsupported,
    #[error("分页正文失败：{0}")]
    View(#[from] ViewError),
    #[error("原字体捕获失败：{0}")]
    Font(#[from] super::paged_fonts::NativeMarkdownPagedFontError),
    #[error("原字体注册失败：{0}")]
    Registration(#[from] super::paged_line_fonts::NativeMarkdownLineFontError),
    #[error("原段落样式捕获失败：{0}")]
    Paragraph(#[from] super::paged_paragraph_style::NativeMarkdownParagraphStyleError),
    #[error("原滚动正文失败：{0}")]
    Viewport(#[from] super::paged_viewport::NativeMarkdownPagedViewportError),
    #[error("原组合器失败：{0}")]
    Compose(#[from] omni_markdown_core::MarkdownLineComposeError),
    #[error("原字体试次失败：{0}")]
    Fallback(#[from] super::paged_font_fallback::NativeMarkdownFontFallbackError),
    #[error("当前正文的有限语义准备失败")]
    Semantics(omni_markdown_core::markdown_document_buffer::MarkdownTableRequestError),
    #[error("原字体试次线程异常结束")]
    WorkerFailed,
    #[error("准备操作与资源归还同时失败：{cause}；另一失败：{cleanup}")]
    PreparationCleanup {
        #[source]
        cause: Box<Self>,
        cleanup: Box<Self>,
    },
    #[error("正文派生资源释放失败，本次编辑未提交")]
    ResourceReleaseFailed,
    #[error("原正文准备请求已经由真实输入或几何变更撤销")]
    PreparationRevoked,
    #[error("原单元格提交失败：{0}")]
    CellCommit(
        super::paged_input::NativeMarkdownPagedCellCommitError<
            super::paged_input::NativeMarkdownPagedInputObserverError,
        >,
    ),
}

impl From<omni_markdown_core::markdown_document_buffer::MarkdownTableRequestError>
    for NativeMarkdownPagedBodyError
{
    /// The Core enum is a complete typed fact rather than std::error::Error; retain it for diagnostics
    /// while UI uses the Chinese message above.
    fn from(
        error: omni_markdown_core::markdown_document_buffer::MarkdownTableRequestError,
    ) -> Self {
        Self::Semantics(error)
    }
}

type Result<T> = std::result::Result<T, NativeMarkdownPagedBodyError>;

/// Assembly transfers actual owners into one Native initialization graph; a raw public range cannot
/// construct a Cell.
enum Attachment {
    Body(Rc<RefCell<MarkdownDocumentBuffer>>),
    Cell(Rc<NativeMarkdownPagedCellSession>),
}

/// The existing UI style supplies colors; this component creates no separate theme and preserves
/// body/selection color semantics.
#[derive(Clone, Copy)]
pub struct NativeMarkdownPagedBodyColors {
    pub background: [f64; 4],
    /// Original UI background for the active Source logical line; drawing neither infers a theme nor
    /// changes text colors.
    pub active_line: [f64; 4],
    pub text: [f64; 4],
    pub selection: [f64; 4],
    pub caret: [f64; 4],
}

/// All fields are finite configuration; point size comes from the actual CTFont without reconstructing
/// a named system font.
pub struct NativeMarkdownPagedBodyConfiguration {
    /// Source preserves literal text; InlinePreview consumes certified RootLeaf font, visibility, and
    /// underline facts without claiming complete GeneralInline support.
    pub presentation_mode: omni_markdown_core::MarkdownPresentationMode,
    pub epoch: u64,
    pub maximum_font_bytes: u64,
    pub padding: f64,
    pub colors: NativeMarkdownPagedBodyColors,
}

/// Base fonts have explicit Native roles without prebuilding another Core palette or layout
/// continuation.
/// Generic attachment uses the caller's same font; only the product editor explicitly supplies separate
/// Source and preview document fonts.
enum BaseFonts {
    Single(CFRetained<CTFont>),
    Presentation {
        source: CFRetained<CTFont>,
        preview: CFRetained<CTFont>,
    },
}
impl BaseFonts {
    /// Select an already-owned font role without allocating or reconstructing a font.
    fn for_mode(&self, mode: omni_markdown_core::MarkdownPresentationMode) -> &CTFont {
        match self {
            Self::Single(font) => font,
            Self::Presentation { source, preview } => match mode {
                omni_markdown_core::MarkdownPresentationMode::Source => source,
                omni_markdown_core::MarkdownPresentationMode::InlinePreview => preview,
            },
        }
    }
}

/// Read-only actual Composer counters cannot establish product acceptance without Ready and independent
/// measurement.
#[derive(Debug)]
pub struct NativeMarkdownPagedBodyProgress {
    pub phase: &'static str,
    pub scheduled_serial: Option<u64>,
    pub producer_serial: u64,
    pub revoked_preparations: usize,
    pub preparing: bool,
    pub metadata_part_cursor: Option<usize>,
    pub scan_cursor_line: Option<usize>,
    pub scan_collected_lines: usize,
    pub font_trial_in_flight: bool,
    pub prepared_lines: usize,
    pub prepared_height: f64,
    pub frontier_source_bytes: usize,
    pub shaped_input_bytes: usize,
    /// Cumulative step reads include streaming bidi/script/grapheme/line-break scans; this is neither
    /// unique Source bytes nor heap capacity.
    pub composer_step_bytes: usize,
    pub composer_step_scalars: usize,
    pub composer_step_metadata_units: usize,
    pub maximum_step_bytes: usize,
    pub maximum_step_scalars: usize,
    pub maximum_step_metadata_units: usize,
    pub trial_source_bytes: usize,
    /// Actual cumulative work of the sole semantic context, separate from Composer accounting and
    /// always zero in Source mode.
    pub semantic_work: omni_markdown_core::markdown_document_buffer::SemanticWork,
    pub source_total_bytes: usize,
    pub complete: bool,
    pub target_lookup_records: usize,
    pub target_metric_parts: usize,
    pub target_walk_lines: usize,
    pub target_index_records: usize,
    /// Only remote-batch lines awaiting actual font metrics; ordinary line-by-line preparation is
    /// excluded from target work.
    pub target_unmeasured_lines: usize,
    /// Actual Source range of the last queued line; reads existing LineReady facts without advancing
    /// layout or retaining its Source object.
    pub target_unmeasured_last_source: Option<std::ops::Range<usize>>,
    pub target_max_batch_lines: usize,
    /// Actual checkpoint calls during target preparation only, excluding previous first-frame or
    /// ordinary scroll checkpoints.
    pub target_checkpoint_attempts: usize,
    /// Accumulated actual checkpoint-entry time, including guards/sync/dup; this does not measure fsync
    /// alone.
    pub target_checkpoint_millis: f64,
    /// Actual 64-byte record reads for batch-end, predecessor, and part access, separate from
    /// old-prefix Y backtracking.
    pub target_batch_index_records: usize,
}

/// Observers of one actual Cell retain only weak identity and atomic facts, without extending display
/// Source, directory FD, or worker lifetimes.
/// None means the role was never created; started display conversion observes its actual writer,
/// including unpublished directories.
pub struct NativeMarkdownPagedCellResourceReceipts {
    /// Observe Source and coordinate-directory ownership during or after display conversion without
    /// creating another reader.
    pub display: Option<omni_markdown_core::MarkdownTableCellDisplayResourceObserver>,
    /// Observe the original writer while a Composer exists; a completed Cell that transferred its
    /// reader does not invent a writer.
    pub writer: Option<omni_markdown_core::MarkdownComposedLineResourceObserver>,
    /// Observe the sealed visible-prefix reader through its final release, including a temporary old
    /// Scene claim on the same resource.
    pub reader: Option<omni_markdown_core::MarkdownComposedLineResourceObserver>,
    /// Observe actual Composer roles; a Cell without a Composer creates no worker merely to fill the
    /// receipt.
    pub worker: Option<omni_markdown_core::MarkdownLineComposerWorkerResourceObserver>,
}

impl NativeMarkdownPagedCellResourceReceipts {
    /// Verify every Cell resource actually acquired: Source payload, projection FD/work pages, layout
    /// directory, and real worker must be returned.
    /// Storage and observation failures remain errors; None means this stage held no role and cannot
    /// certify unobserved historical resources.
    pub fn verify_released(&self) -> std::result::Result<(), String> {
        if let Some(display) = &self.display {
            let actual = display.snapshot();
            if !actual.source_payload_released
                || actual.origin_file_open
                || actual.origin_buffer_bytes != 0
                || actual.origin_cleanup_failed
                || actual.origin_reader_failed
            {
                return Err(format!("原表格显示Source/投影目录未归还：{actual:?}"));
            }
        }
        // Reader and writer observe their actual original directories; disappearance of the Cell or a
        // cancellation flag cannot prove FD/mapping return.
        for observer in self.reader.iter().chain(self.writer.iter()) {
            let actual = observer.snapshot();
            if actual.file_open
                || actual.mapped_bytes != 0
                || actual.buffer_bytes != 0
                || actual.cleanup_failed
                || actual.reader_failed
                || !actual.source_payload_released
                || actual.first_source_errno.is_some()
            {
                return Err(format!("原表格Reader/Writer资源未归还：{actual:?}"));
            }
        }
        if let Some(worker) = &self.worker {
            let actual = worker.snapshot().ok_or("原表格worker观察者已poison")?;
            if !actual.is_released() {
                // The original worker already covers each actual FD/mapping/Source role; do not
                // introduce another role ledger here.
                return Err(format!("原表格worker资源未归还：{actual:?}"));
            }
        }
        Ok(())
    }
}

/// Fixed observers of the constructed resources hold no document, Source, font, or file strong
/// references.
pub struct NativeMarkdownPagedBodyResourceReceipts {
    pub source: omni_markdown_core::MarkdownLayoutTextSourceObserver,
    /// None before any prefix reader exists; an original pending reader under measurement still returns
    /// its actual observer.
    pub reader: Option<omni_markdown_core::MarkdownComposedLineResourceObserver>,
    pub writer: omni_markdown_core::MarkdownComposedLineResourceObserver,
    pub worker: omni_markdown_core::MarkdownLineComposerWorkerResourceObserver,
    pub fonts: Vec<(
        omni_markdown_core::MarkdownLayoutFontBytesResourceObserver,
        omni_markdown_core::MarkdownLayoutFontResourceObserver,
    )>,
    /// Observe current, visible, and restoring Grid Cells without retaining history, copying Source, or
    /// creating readers.
    pub cells: Vec<NativeMarkdownPagedCellResourceReceipts>,
}

/// The current window observes receipts already published by its original Input; notifications retain
/// no text and certify neither pixels nor disk-save completion.
pub trait NativeMarkdownPagedBodyInputObserver {
    fn committed(&self, commit: &NativeMarkdownPagedInputCommit);
    /// Local draft input and client switching update toolbar facts only; they cannot masquerade as
    /// canonical edits that trigger save.
    fn local_input_changed(&self);
}

/// The composition root owns the Body Rc; View delegation and async continuations retain weak
/// references only.
pub struct NativeMarkdownPagedBody {
    weak: Weak<Self>,
    document: RefCell<Option<Rc<RefCell<MarkdownDocumentBuffer>>>>,
    cell_session: RefCell<Option<Rc<NativeMarkdownPagedCellSession>>>,
    input: Rc<NativeMarkdownPagedInput>,
    view: RefCell<Option<Rc<Retained<NativeMarkdownPagedTextView>>>>,
    viewport: RefCell<Option<Rc<NativeMarkdownPagedViewport>>>,
    gutter: RefCell<Option<Rc<Retained<super::gutter::paged::NativeMarkdownPagedGutter>>>>,
    notifications: RefCell<
        Vec<Retained<objc2::runtime::ProtocolObject<dyn objc2_foundation::NSObjectProtocol>>>,
    >,
    off_main_notice: std::sync::Arc<std::sync::atomic::AtomicBool>,
    fonts: RefCell<Vec<Rc<NativeMarkdownLineFont>>>,
    // Rc lends immutable Native fonts outside slot borrows during synchronous assembly; close removes
    // the actual strong claims.
    base_fonts: RefCell<Option<Rc<BaseFonts>>>,
    // Reuse completed choices for the same actual font and complete small grapheme only; weak font
    // tickets do not extend palette resource lifetimes.
    font_choices: RefCell<font_choice::FontChoices>,
    typography: RefCell<Option<semantics::Typography>>,
    catalog: MarkdownLineFontCatalog,
    // Parent and child borrow the same captured paragraph facts; active Cells neither reread system
    // style nor copy font files.
    paragraph: Rc<super::paged_paragraph_style::NativeMarkdownLineParagraph>,
    active_cell: RefCell<Option<Rc<cell::ActiveCell>>>,
    cell_close: RefCell<Option<Rc<NativeMarkdownPagedCloseReceipt>>>,
    cell_parent: RefCell<Weak<Self>>,
    pending_cell: RefCell<Option<Rc<cell::PendingCell>>>,
    // Allocate the original continuation once; each turn moves its sole Box out and back instead of
    // moving the entire layout state.
    producer: RefCell<Option<Box<Producer>>>,
    close_receipt: RefCell<Option<Rc<NativeMarkdownPagedCloseReceipt>>>,
    // Retain only observers for the last actual retired Source/directory/worker/style-font set, never a
    // history of Native graphs.
    mode_retirement: RefCell<Option<NativeMarkdownPagedBodyResourceReceipts>>,
    worker: RefCell<Option<FontWorker>>,
    scene: RefCell<Option<Rc<Scene>>>,
    source_target: RefCell<Option<Rc<target::SourceTargetRequest>>>,
    mode_viewport: RefCell<Option<mode::PendingModeViewport>>,
    // A button click retains one original weak intent; Pending layout neither resigns input nor changes
    // the actual mode early.
    pending_mode_action: RefCell<Option<Rc<mode::PendingModeAction>>>,
    // Each actual button click revokes its predecessor without advancing document/layout epochs or
    // certifying switch completion.
    mode_action_revision: Cell<u64>,
    selection_reveal: RefCell<Option<NativeMarkdownPagedSceneRequest>>,
    pending_vertical: RefCell<Option<Rc<navigation::PendingVertical>>>,
    pending_page_scroll: RefCell<Option<Rc<page_scroll::PendingPageScroll>>>,
    pending_find: RefCell<Option<Rc<find::PendingFind>>>,
    // Bar and controller bind weakly; a Scene holds only current finite matches, keeping query and
    // Native owners out of the glyph directory.
    find_highlights: RefCell<Option<find_highlights::Binding>>,
    pending_link: RefCell<Option<link::PendingLink>>,
    pending_word_delete: RefCell<Option<Rc<word::PendingWordDelete>>>,
    input_observers: RefCell<Vec<Weak<dyn NativeMarkdownPagedBodyInputObserver>>>,
    paint_observation: RefCell<Option<PaintObservation>>,
    #[cfg(feature = "native-markdown-runtime-probe")]
    preparation_probe: RefCell<Option<Weak<dyn NativeMarkdownPagedBodyPreparationProbe>>>,
    revoked_preparations: Cell<usize>,
    // The current body session owns its mode; configuration supplies only the initial value, and
    // switching preserves Input, history, and folds.
    presentation: Cell<omni_markdown_core::MarkdownPresentationMode>,
    configuration: NativeMarkdownPagedBodyConfiguration,
    active: Cell<bool>,
    failed: Cell<bool>,
    scheduled: Cell<Option<u64>>,
    preparing: Cell<bool>,
    deferred: Cell<bool>,
    font_notices: RefCell<VecDeque<u64>>,
    font_notice_failed: Cell<bool>,
    serial: Cell<u64>,
    layout_epoch: Cell<u64>,
    failure: RefCell<Option<NativeMarkdownPagedBodyError>>,
    mtm: MainThreadMarker,
}

impl NativeMarkdownPagedBody {
    /// Window UI and successor Producers read the actual mode owned by this same body session.
    pub(in crate::native_surface) fn presentation_mode(
        &self,
    ) -> omni_markdown_core::MarkdownPresentationMode {
        self.presentation.get()
    }

    /// Transfer the original document owner into its NSScrollView; failure returns the partial
    /// candidate through existing release ownership.
    ///
    /// After business close, the caller must still verify Native weak identities, mappings, and the
    /// close receipt. This method mounts only the supplied scroll root;
    /// configuration and subsequent same-Body transactions own presentation mode, without new windows,
    /// shortcuts, or title bars.
    pub fn attach(
        document: Rc<RefCell<MarkdownDocumentBuffer>>,
        scroll: &NSScrollView,
        font: &CTFont,
        paragraph: &NSParagraphStyle,
        configuration: NativeMarkdownPagedBodyConfiguration,
        mtm: MainThreadMarker,
    ) -> Result<Rc<Self>> {
        Self::attach_target(
            Attachment::Body(document),
            scroll,
            Rc::new(BaseFonts::Single(font.retain())),
            paragraph,
            configuration,
            mtm,
        )
    }

    /// The product editor explicitly distinguishes two document roles while ordinary attach retains its
    /// single-font contract.
    /// Capture one Core font for the initial mode; the other role retains only its Native font claim.
    pub(in crate::native_surface) fn attach_with_presentation_fonts(
        document: Rc<RefCell<MarkdownDocumentBuffer>>,
        scroll: &NSScrollView,
        source_font: &CTFont,
        preview_font: &CTFont,
        paragraph: &NSParagraphStyle,
        configuration: NativeMarkdownPagedBodyConfiguration,
        mtm: MainThreadMarker,
    ) -> Result<Rc<Self>> {
        Self::attach_target(
            Attachment::Body(document),
            scroll,
            Rc::new(BaseFonts::Presentation {
                source: source_font.retain(),
                preview: preview_font.retain(),
            }),
            paragraph,
            configuration,
            mtm,
        )
    }

    /// Render an actual Cell session through the same viewport; the outer model retains the document
    /// without copied drafts or hidden TextKit.
    pub fn attach_cell(
        session: Rc<NativeMarkdownPagedCellSession>,
        scroll: &NSScrollView,
        font: &CTFont,
        paragraph: &NSParagraphStyle,
        configuration: NativeMarkdownPagedBodyConfiguration,
        mtm: MainThreadMarker,
    ) -> Result<Rc<Self>> {
        Self::attach_target(
            Attachment::Cell(session),
            scroll,
            Rc::new(BaseFonts::Single(font.retain())),
            paragraph,
            configuration,
            mtm,
        )
    }

    /// Capture one actual font/paragraph set before transferring it into the sole mount graph.
    fn attach_target(
        target: Attachment,
        scroll: &NSScrollView,
        base_fonts: Rc<BaseFonts>,
        paragraph: &NSParagraphStyle,
        configuration: NativeMarkdownPagedBodyConfiguration,
        mtm: MainThreadMarker,
    ) -> Result<Rc<Self>> {
        // Reject invalid configuration before Native graph creation; nonfinite colors cannot silently
        // clamp into a different theme.
        if configuration.epoch == 0
            || !configuration.padding.is_finite()
            || configuration.padding < 0.0
            || configuration
                .colors
                .background
                .into_iter()
                .chain(configuration.colors.active_line)
                .chain(configuration.colors.text)
                .chain(configuration.colors.selection)
                .chain(configuration.colors.caret)
                .any(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
        {
            return Err(ViewError::InvalidGeometry.into());
        }
        let font = base_fonts.for_mode(configuration.presentation_mode);
        let captured =
            NativeMarkdownPagedFont::capture(font, configuration.maximum_font_bytes, mtm, &|| {
                true
            })?;
        let catalog = MarkdownLineFontCatalog::new();
        let paragraph = Rc::new(super::paged_paragraph_style::capture_paged_paragraph_style(
            paragraph,
            None,
            mtm,
            &|| true,
        )?);
        let typography = (configuration.presentation_mode
            == omni_markdown_core::MarkdownPresentationMode::InlinePreview)
            .then(|| semantics::Typography::new(font))
            .transpose()?;
        let registered = NativeMarkdownLineFont::register(captured, &catalog, &|| true)?;
        let registered = Rc::new(if let Some(typography) = &typography {
            registered.capture_preview_metrics(&typography.metrics_owner, &|| true)?
        } else {
            registered // Source retains original Core Text line height/baseline without constructing a system paragraph-metrics owner.
        });
        Self::mount_target(
            target,
            scroll,
            vec![registered],
            base_fonts,
            catalog,
            paragraph,
            typography,
            configuration,
        )
    }

    /// Both input types construct their Native graph only here. Active Cells borrow their parent's
    /// actual registered font and paragraph
    /// and share viewport, notification, input, and failure cleanup with public Cell attachment rather
    /// than duplicating the mount flow.
    #[expect(
        clippy::too_many_arguments,
        reason = "唯一挂载边明确接管输入、字体、段落与视口资源，不以参数袋隐藏实际所有权"
    )]
    fn mount_target(
        target: Attachment,
        scroll: &NSScrollView,
        fonts: Vec<Rc<NativeMarkdownLineFont>>,
        base_fonts: Rc<BaseFonts>,
        catalog: MarkdownLineFontCatalog,
        paragraph: Rc<super::paged_paragraph_style::NativeMarkdownLineParagraph>,
        typography: Option<semantics::Typography>,
        configuration: NativeMarkdownPagedBodyConfiguration,
    ) -> Result<Rc<Self>> {
        let mtm = scroll.mtm();
        let (document, cell_session, input) = match target {
            Attachment::Body(document) => {
                let input = NativeMarkdownPagedInput::new(Rc::downgrade(&document))
                    .map_err(ViewError::Input)?;
                (Some(document), None, input)
            }
            Attachment::Cell(session) => {
                if configuration.presentation_mode
                    != omni_markdown_core::MarkdownPresentationMode::Source
                {
                    return Err(NativeMarkdownPagedBodyError::Unsupported);
                } // Draft-local coordinates cannot borrow RootLeaf semantics from the original document.
                let input = NativeMarkdownPagedInput::new_cell(Rc::downgrade(&session))
                    .map_err(ViewError::Input)?;
                (None, Some(session), input)
            }
        };
        let owner = Rc::new_cyclic(|weak| Self {
            weak: weak.clone(),
            document: RefCell::new(document),
            cell_session: RefCell::new(cell_session),
            input,
            view: RefCell::new(None),
            viewport: RefCell::new(None),
            gutter: RefCell::new(None),
            notifications: RefCell::new(Vec::new()),
            off_main_notice: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            fonts: RefCell::new(fonts),
            base_fonts: RefCell::new(Some(base_fonts)),
            font_choices: RefCell::new(font_choice::FontChoices::new()),
            typography: RefCell::new(typography),
            catalog,
            paragraph,
            active_cell: RefCell::new(None),
            cell_close: RefCell::new(None),
            cell_parent: RefCell::new(Weak::new()),
            pending_cell: RefCell::new(None),
            producer: RefCell::new(None),
            close_receipt: RefCell::new(None),
            mode_retirement: RefCell::new(None),
            worker: RefCell::new(None),
            scene: RefCell::new(None),
            source_target: RefCell::new(None),
            mode_viewport: RefCell::new(None),
            pending_mode_action: RefCell::new(None),
            mode_action_revision: Cell::new(0),
            selection_reveal: RefCell::new(None),
            pending_vertical: RefCell::new(None),
            pending_page_scroll: RefCell::new(None),
            pending_find: RefCell::new(None),
            find_highlights: RefCell::new(None),
            pending_link: RefCell::new(None),
            pending_word_delete: RefCell::new(None),
            input_observers: RefCell::new(Vec::new()),
            paint_observation: RefCell::new(None),
            #[cfg(feature = "native-markdown-runtime-probe")]
            preparation_probe: RefCell::new(None),
            revoked_preparations: Cell::new(0),
            presentation: Cell::new(configuration.presentation_mode),
            configuration,
            active: Cell::new(true),
            failed: Cell::new(false),
            scheduled: Cell::new(None),
            preparing: Cell::new(false),
            deferred: Cell::new(false),
            font_notices: RefCell::new(VecDeque::new()),
            font_notice_failed: Cell::new(false),
            serial: Cell::new(1),
            layout_epoch: Cell::new(1),
            failure: RefCell::new(None),
            mtm,
        });
        let erased: Rc<dyn NativeMarkdownPagedTextViewDelegate> = owner.clone();
        let frame = scroll.contentView().bounds();
        let view = Rc::new(NativeMarkdownPagedTextView::new_with_render_strategy(
            owner.input.clone(),
            Rc::downgrade(&erased),
            owner.configuration.epoch,
            frame,
            NativeMarkdownPagedRenderStrategy::CpuViewport,
            mtm,
        )?);
        *owner.view.borrow_mut() = Some(view.clone());
        if owner.document.borrow().is_some()
            && owner.presentation_mode() == omni_markdown_core::MarkdownPresentationMode::Source
        {
            // Reserve actual ruler width before mounting the body; original Source/Cell padding is
            // independent of this layout.
            let lines = owner
                .document
                .borrow()
                .as_ref()
                .expect("Body持有canonical文档")
                .borrow()
                .line_count();
            let gutter = Rc::new(super::gutter::paged::NativeMarkdownPagedGutter::new(
                owner.weak.clone(),
                scroll,
                &view,
                lines,
                mtm,
            )?);
            *owner.gutter.borrow_mut() = Some(gutter.clone());
            gutter.mount()?;
        }
        // Initial height uses only the actual clip's minimum visible range, without estimating from
        // character count or claiming whole-document height.
        let viewport = NativeMarkdownPagedViewport::attach(scroll, &view, frame.size.height, mtm)?;
        *owner.viewport.borrow_mut() = Some(viewport);
        owner.observe_clip(&scroll.contentView());
        owner.schedule()?;
        Ok(owner)
    }

    /// Borrow the actual body View temporarily; a caller must not retain it in a close-completion
    /// ticket and extend business resources.
    pub fn view(&self) -> Result<Rc<Retained<NativeMarkdownPagedTextView>>> {
        self.view
            .borrow()
            .clone()
            .ok_or(NativeMarkdownPagedBodyError::Closed)
    }

    /// Save, history state, and input observation share the actual commit boundary; additional weak
    /// observers do not replace existing consumers.
    pub fn observe_input(
        &self,
        observer: Weak<dyn NativeMarkdownPagedBodyInputObserver>,
    ) -> Result<()> {
        if !self.current_body() {
            return Err(NativeMarkdownPagedBodyError::Closed);
        }
        let mut observers = self.input_observers.borrow_mut();
        // Remove departed input consumers and retain only live weak subscriptions, without caching
        // receipts or document text.
        observers.retain(|registered| registered.strong_count() != 0);
        if !observers
            .iter()
            .any(|registered| Weak::ptr_eq(registered, &observer))
        {
            observers.push(observer);
        }
        Ok(())
    }

    /// Save or mode transfer cannot overtake admitted unpublished keyboard actions; ordinary pixel
    /// preparation is not an unfinished edit.
    pub(in crate::native_surface) fn has_pending_input(&self) -> bool {
        self.pending_word_delete.borrow().is_some() || self.pending_vertical.borrow().is_some()
    }

    /// Position a newly mounted actual mode using current complete Input authority, without synthetic
    /// selection commits or undo entries.
    pub(in crate::native_surface) fn reveal_current_selection(&self) -> Result<()> {
        let request = self.view()?.capture_scene_request()?;
        if !self.current_body() || !self.input.state().is_ok_and(|state| state == request.input) {
            return Err(ViewError::Revoked.into());
        }
        *self.selection_reveal.borrow_mut() = Some(request);
        self.schedule()
    }

    /// Transfer the first failure to its final handling boundary once.
    pub fn take_failure(&self) -> Option<NativeMarkdownPagedBodyError> {
        self.failure.borrow_mut().take()
    }

    /// Consume the original role ticket before capture; finite live fonts serve
    /// actual-instance/frozen-mapping reuse without retaining an old Scene.
    /// End all Body slot borrows before Native calls; the original caller preserves the first Source
    /// and resource failures.
    fn capture_base_font(
        &self,
        mode: omni_markdown_core::MarkdownPresentationMode,
        current: &dyn Fn() -> bool,
    ) -> Result<NativeMarkdownPagedFont> {
        let base_fonts = self
            .base_fonts
            .borrow()
            .as_ref()
            .cloned()
            .ok_or(NativeMarkdownPagedBodyError::Closed)?;
        let palette = self.fonts.borrow().clone();
        let captured = NativeMarkdownPagedFont::capture_reusing(
            base_fonts.for_mode(mode),
            &palette,
            self.configuration.maximum_font_bytes,
            self.mtm,
            current,
        )?;
        drop(palette);
        if !current() {
            return Err(ViewError::Revoked.into());
        }
        Ok(captured.into_font())
    }

    /// Actual sealed-prefix height only; it does not represent the document tail that has not been laid
    /// out.
    pub fn prepared_height(&self) -> Option<f64> {
        self.producer
            .borrow()
            .as_ref()
            .map(|producer| producer.height())
    }

    /// Observe existing production work without advancing layout or acquiring a new Source.
    pub fn progress(&self) -> Result<NativeMarkdownPagedBodyProgress> {
        let mut progress = self
            .producer
            .borrow()
            .as_ref()
            .ok_or(NativeMarkdownPagedBodyError::Busy)?
            .progress()?;
        progress.scheduled_serial = self.scheduled.get();
        progress.producer_serial = self.serial.get();
        progress.revoked_preparations = self.revoked_preparations.get();
        progress.preparing = self.preparing.get();
        Ok(progress)
    }

    /// Borrow resource observers from actual owners without retaining their payloads.
    pub fn resource_receipts(&self) -> Result<NativeMarkdownPagedBodyResourceReceipts> {
        let mut receipts = self
            .producer
            .borrow()
            .as_ref()
            .ok_or(NativeMarkdownPagedBodyError::Busy)?
            .receipts()?;
        receipts.fonts = self
            .fonts
            .borrow()
            .iter()
            .map(|font| {
                (
                    font.native().bytes_observer(),
                    font.native().core_font().resource_observer(),
                )
            })
            .collect();
        Ok(receipts)
    }

    /// Report queued, synchronous, or delivered-worker work still owned by this Body.
    pub fn is_preparing(&self) -> bool {
        self.scheduled.get().is_some()
            || self.preparing.get()
            || self
                .producer
                .borrow()
                .as_ref()
                .is_some_and(|producer| producer.has_in_flight())
    }

    /// Foreground authority ends after explicit close, sticky failure, or an off-main notice.
    fn current_body(&self) -> bool {
        self.active.get()
            && !self.failed.get()
            && !self
                .off_main_notice
                .load(std::sync::atomic::Ordering::Acquire)
    }

    /// Coalesce this body's next real preparation request without waking main during its own work.
    fn schedule(&self) -> Result<()> {
        if !self.current_body() {
            // A released or failed owner cannot acquire another dispatch callback.
            return Err(NativeMarkdownPagedBodyError::Closed);
        }
        let serial = self.serial.get();
        if self.scheduled.get() == Some(serial) {
            // The existing callback already owns this exact epoch's continuation.
            return Ok(());
        }
        if self.preparing.get() {
            // Preparation still owns the synchronous stack. Record actual requested work;
            // its exit either consumes another bounded turn or enqueues one successor.
            self.deferred.set(true);
            return Ok(());
        }
        self.scheduled.set(Some(serial));
        let weak = MainThreadBound::new(self.weak.clone(), self.mtm);
        super::enqueue_native_markdown_main_task(move || {
            let mtm = MainThreadMarker::new().expect("body continuations execute on AppKit main");
            let Some(owner) = weak.get(mtm).upgrade() else {
                // Queued work lends no lifetime to a body that has already closed.
                return;
            };
            if owner.serial.get() != serial
                || owner.scheduled.get() != Some(serial)
                || !owner.active.get()
            {
                // A stale callback cannot consume or execute a newer epoch's scheduled request.
                return;
            }
            owner.scheduled.set(None);
            owner.run_preparation_turn(serial);
        });
        Ok(())
    }

    /// Spend a short main-thread slice on already-requested work, preserving every original step
    /// boundary.
    fn run_preparation_turn(&self, serial: u64) {
        const MAX_TURNS: usize = 8;
        const TIME_SLICE: std::time::Duration = std::time::Duration::from_millis(2);
        let started = std::time::Instant::now();
        for turn in 0..MAX_TURNS {
            if self.word_input_is_copying() {
                // A real payload getter owns completion; do not poll it or drain empty callbacks.
                return;
            }
            if self.preparing.replace(true) {
                // Native reentry leaves the original stack in charge of releasing this busy claim.
                self.deferred.set(true);
                return;
            }
            // Each iteration still captures its real Native geometry and retains the original
            // Core/source budgets. Batching removes queue hops, never validation or bounded reads.
            let result = self.advance();
            self.preparing.set(false);
            if let Err(cause) = result {
                // Preserve the first actual preparation failure and stop this slice immediately.
                self.fail(cause);
                return;
            }
            if let Err(cause) = self.finish_cell_work() {
                // Canonical cell actions run only after preparation and its slot borrows have ended.
                self.fail(cause);
                return;
            }
            if self.font_notice_failed.replace(false) {
                // An observed worker delivery failure cannot be replaced with guessed progress.
                self.fail(NativeMarkdownPagedBodyError::WorkerFailed);
                return;
            }
            // Consume only delivered results, between the same original preparation boundaries.
            // No receiver polling or native-resource wait is introduced by this short slice.
            let notices = std::mem::take(&mut *self.font_notices.borrow_mut());
            for notice in notices {
                if let Err(cause) = self.finish_font_trial(notice) {
                    self.fail(cause);
                    return;
                }
            }
            let requested = self.deferred.replace(false);
            if !requested {
                // No actual continuation request means there is nothing to batch or enqueue.
                break;
            }
            let copying = self.word_input_is_copying();
            let can_continue = turn + 1 < MAX_TURNS
                && started.elapsed() < TIME_SLICE
                && self.current_body()
                && self.serial.get() == serial
                && self.scheduled.get().is_none()
                && self.scene.borrow().is_none()
                && !copying;
            if can_continue {
                // Only this unchanged owner with no ready Scene or queued successor may spend
                // the remaining slice. A new epoch and Native painting always regain the queue.
                continue;
            }
            if !copying && self.current_body() {
                // Yield after the time/work cap, scene publication, or epoch handoff. An already
                // queued successor coalesces here; a real payload getter resumes itself instead.
                if let Err(cause) = self.schedule() {
                    self.fail(cause);
                    return;
                }
            }
            break;
        }
        // Mode delivery remains outside preparation and all state borrows, once per callback.
        // Only an actually published original Scene can authorize its pending Native action.
        self.deliver_pending_mode_action();
    }

    /// Preserve the first typed failure, revoke publication, and retire all actual owned work.
    fn fail(&self, cause: NativeMarkdownPagedBodyError) {
        self.fail_pending_find(&cause);
        // The first failure is sticky: stop the original continuation and clear Ready without capturing
        // a newer Source to hide the failure.
        if self.failure.borrow().is_none() {
            #[cfg(feature = "native-markdown-runtime-probe")]
            {
                // Record enum classes only at the original first-failure boundary; later Busy/Revoked
                // cannot replace its cause.
                // Read only the typed cause already held here; do not traverse chains or expand
                // strings, Source, or document paths.
                // A View error may contain link or attachment text, so never Debug-format the complete
                // ViewError.
                let view = match &cause {
                    NativeMarkdownPagedBodyError::View(error) => {
                        Some(std::mem::discriminant(error))
                    }
                    _ => None,
                };
                // Core classes distinguish authority, coverage, line breaks, and shaping; concrete
                // Source and page resources remain in the original error.
                let compose = match &cause {
                    NativeMarkdownPagedBodyError::Compose(error) => {
                        Some(std::mem::discriminant(error))
                    }
                    _ => None,
                };
                // Font capture may carry system/file failures; identify only its class without printing
                // descriptions or URLs.
                let font = match &cause {
                    NativeMarkdownPagedBodyError::Font(error) => {
                        Some(std::mem::discriminant(error))
                    }
                    _ => None,
                };
                // Registration failure belongs to the actual catalog; diagnostics do not reparse,
                // reregister, or acquire font authority.
                let registration = match &cause {
                    NativeMarkdownPagedBodyError::Registration(error) => {
                        Some(std::mem::discriminant(error))
                    }
                    _ => None,
                };
                // Read only the captured paragraph error class; do not call Native style getters again
                // at the failure boundary.
                let paragraph = match &cause {
                    NativeMarkdownPagedBodyError::Paragraph(error) => {
                        Some(std::mem::discriminant(error))
                    }
                    _ => None,
                };
                // Distinguish scroll geometry from body Source failure without recapturing a Scene or
                // acquiring a successor window.
                let viewport = match &cause {
                    NativeMarkdownPagedBodyError::Viewport(error) => {
                        Some(std::mem::discriminant(error))
                    }
                    _ => None,
                };
                // Observation cannot advance a candidate trial; its original cause retains complete
                // inner failures and release obligations.
                let fallback = match &cause {
                    NativeMarkdownPagedBodyError::Fallback(error) => {
                        Some(std::mem::discriminant(error))
                    }
                    _ => None,
                };
                // Preserve WorkLimit/revocation/Source classes without outputting any frozen-Source
                // error payload.
                let semantics = match &cause {
                    NativeMarkdownPagedBodyError::Semantics(error) => {
                        Some(std::mem::discriminant(error))
                    }
                    _ => None,
                };
                // Project errno only from an actual std::io::Error without formatting a message that
                // may contain external payload.
                let storage_errno = match &cause {
                    NativeMarkdownPagedBodyError::Compose(
                        omni_markdown_core::MarkdownLineComposeError::Storage(error),
                    ) => error.raw_os_error(),
                    _ => None,
                };
                // Fixed fields contain only discriminants and optional integers; normal builds compile
                // out this diagnostic.
                // Original sticky assignment and real cleanup still run unchanged; diagnostics neither
                // consume nor replace any error.
                eprintln!(
                    "native_body_failure body={:?} view={view:?} compose={compose:?} font={font:?} registration={registration:?} paragraph={paragraph:?} viewport={viewport:?} fallback={fallback:?} semantics={semantics:?} storage_errno={storage_errno:?}",
                    std::mem::discriminant(&cause),
                );
            }
            *self.failure.borrow_mut() = Some(cause);
        }
        self.failed.set(true);
        let pending_mode = self.pending_mode_action.borrow_mut().take();
        drop(pending_mode); // Failure stops mode clicks and releases old Native weak state outside the slot borrow.
        let find_binding = self.find_highlights.borrow_mut().take();
        drop(find_binding); // Native weak release may reenter; never release platform control blocks while a Body RefMut is held.
        self.font_choices.borrow_mut().clear(); // Failure returns original grapheme/context bytes instead of retaining them in an idle Body.
        drop(self.mode_viewport.borrow_mut().take());
        drop(self.selection_reveal.borrow_mut().take());
        drop(self.pending_vertical.borrow_mut().take());
        drop(self.pending_page_scroll.borrow_mut().take());
        let _ = self.cancel_word_delete(); // The first error is preserved; explicitly return input slots that a local reentrant Rc might still retain.
        self.revoke_source_target();
        drop(self.paint_observation.borrow_mut().take());
        #[cfg(feature = "native-markdown-runtime-probe")]
        drop(self.preparation_probe.borrow_mut().take());
        let scene_result = self.clear_scene().map_err(Into::into);
        let producer = self.producer.borrow_mut().take();
        let producer_result = producer::merge_preparation_cleanup(
            scene_result,
            producer.map_or(Ok(()), Producer::retire),
        );
        // Failure still cleans the complete worker and queued results; preserve the primary cause and
        // append actual cleanup failure to the same chain.
        let worker_result = self
            .worker
            .borrow_mut()
            .take()
            .map_or(Ok(()), FontWorker::close);
        let cleanup = producer::merge_preparation_cleanup(producer_result, worker_result);
        if cleanup.is_err() {
            let cause = self
                .failure
                .borrow_mut()
                .take()
                .expect("上方已保存首个准备失败");
            *self.failure.borrow_mut() =
                producer::merge_preparation_cleanup(Err(cause), cleanup).err();
        }
    }

    /// Advance one original bounded production quantum, revalidating Source and Native boundaries.
    fn advance(&self) -> Result<()> {
        if self.mode_viewport_waits_for_mount() {
            return Ok(()); // Original bind resumes the sole schedule after Native mounting; do not poll or publish an intermediate document-start frame.
        }
        if self.word_input_is_copying() {
            return Ok(()); // The original Native payload conversion is still on-stack; only its actual completion resumes scheduling.
        }
        let gutter = (self.presentation_mode()
            == omni_markdown_core::MarkdownPresentationMode::Source)
            .then(|| self.gutter.borrow().clone())
            .flatten();
        if let Some(gutter) = gutter {
            let lines = self
                .document
                .borrow()
                .as_ref()
                .expect("Source行号属于原Body文档")
                .borrow()
                .line_count();
            if gutter.update_width(lines)? {
                let viewport = self
                    .viewport
                    .borrow()
                    .as_ref()
                    .cloned()
                    .expect("Source行号更新时原滚动正文已经装配");
                // After the ruler updates its tile, synchronize the sole bounded body with the actual
                // clip; end all slot borrows before reentrant Native geometry.
                viewport.synchronize()?;
                self.schedule()?;
                return Ok(()); // A digit-width boundary changed the actual clip; capture the request next turn without mixing old and new geometry.
            }
        }
        let view = self.view()?;
        let request = view.capture_scene_request().inspect_err(|cause| {
            // Failure diagnostics read the original typed cause without recapturing geometry or
            // reentering Native for logging.
            #[cfg(feature = "native-close-stage-diagnostic")]
            eprintln!(
                "native_body_phase_failure phase=request cause={:?}",
                std::mem::discriminant(cause)
            );
            let _ = cause;
        })?;
        if !self
            .input
            .target_domain()
            .map_err(ViewError::Input)?
            .same(&request.target)
        {
            return Err(ViewError::Revoked.into());
        }
        if self.advance_link(&request)? {
            return Ok(()); // An actual click on the old cache first advances its sole semantic continuation; retain the sealed Scene without another layout/font graph.
        }
        self.refresh_source_target(&request);
        self.refresh_pending_find(&request)?;
        self.refresh_vertical_navigation(&request);
        self.refresh_page_scroll(&request);
        self.refresh_word_delete(&request)?;
        if self.advance_word_typing()? {
            return Ok(()); // Text already committed under the previous receipt; this turn neither advances another Core transaction nor draws intermediate state.
        }
        let scene = self.scene.borrow().clone();
        if let Some(scene) = scene
            && scene.key().request.same(&request)
            && self.scene_current(scene.key())
            && !self.source_target_pending()
            && !self.find_pending()
            && self.selection_reveal.borrow().is_none()
            && self.pending_vertical.borrow().is_none()
            && self.pending_page_scroll.borrow().is_none()
            && self.pending_word_delete.borrow().is_none()
            // Tab/Cell switching admits local work while the parent Input and geometry retain their
            // complete original ticket before commit.
            // Navigation, remount, and close must advance the sole Producer's original Source request;
            // reusing an old frame must not starve the action.
            // Pointer waits only for actual child Ready without borrowing parent Source, so it may
            // reuse the parent frame to avoid duplicate layout.
            && self.pending_cell.borrow().as_ref().is_none_or(|pending| {
                matches!(&**pending, cell::PendingCell::Pointer { .. })
            })
        {
            scene.validate_ready(&|| self.current_body())?;
            return Ok(()); // Repeated clip notices reuse the same-ticket window rather than repeatedly changing nonce and cancelling an in-flight paint candidate.
        }
        // Return an invalid business Scene reader before extending a new prefix. The complete displayed
        // CPU frame retains only
        // its original weak key and pixels; retaining the old image needs neither an old Source/reader
        // nor a third snapshot.
        let selection_scene = self.scene.borrow().clone();
        let selecting_word = self
            .pending_word_delete
            .borrow()
            .as_ref()
            .is_some_and(|pending| pending.selecting());
        let keep_word_selection_scene = selecting_word
            && selection_scene.as_ref().is_some_and(|scene| {
                scene.key().request.same(&request) && self.scene_current(scene.key())
            });
        drop(selection_scene); // Compare only the original same-ticket Scene, releasing the temporary claim before Source/reader transfer.
        if !keep_word_selection_scene {
            self.clear_scene()?;
        } // While word selection leaves text unchanged, keep the actual hit Scene for drag; edits, geometry changes, and deletion still return the old reader.
        let old = self.producer.borrow_mut().take();
        let mut producer = match old {
            Some(producer) if producer.accepts(&request) => producer,
            old => {
                if let Some(old) = old {
                    old.retire()?; // Actually return the old trial and delivered results; late results from the same worker still use their original serial.
                }
                if self.input.is_cell() {
                    self.release_cell_readers()?;
                    if self.input.state().map_err(ViewError::Input)? != request.input {
                        return self.schedule(); // Reentry during actual release produced successor input; the old request cannot authorize a new Source.
                    }
                }
                self.serial.set(
                    self.serial
                        .get()
                        .checked_add(1)
                        .ok_or(NativeMarkdownPagedBodyError::CounterExhausted)?,
                );
                self.layout_epoch.set(
                    self.layout_epoch
                        .get()
                        .checked_add(1)
                        .ok_or(NativeMarkdownPagedBodyError::CounterExhausted)?,
                );
                self.input.clear_navigation_geometry(); // A new Source/width requires actual wrap-side and target-column geometry while preserving logical selection and history.
                // The same original Input permit selects its existing Body/Cell factory; the old
                // Producer is gone, avoiding a third Cell reader.
                let current = |epoch| self.current(epoch);
                let permit = self
                    .input
                    .capture(self.configuration.epoch, &current)
                    .map_err(ViewError::Input)?;
                let source = self
                    .input
                    .capture_layout_source(&permit, &current)
                    .map_err(ViewError::Input)?;
                let folds = if self.presentation_mode()
                    == omni_markdown_core::MarkdownPresentationMode::Source
                    && !self.input.is_cell()
                {
                    self.input
                        .folded_layout_ranges(&permit, &current)
                        .map_err(ViewError::Input)?
                } else {
                    Vec::new()
                }; // Cell and preview do not inherit Body Source folds or acquire its paragraph proof.
                Box::new(Producer::new(
                    source,
                    &request,
                    self.catalog.clone(),
                    self.configuration.padding,
                    self.serial.get(),
                    self.presentation_mode(),
                    &folds,
                )?)
            }
        };
        // Initialize mode from the sole Producer's original Source before first Ready instead of
        // publishing document start and then chasing an old caret.
        if let Err(cause) = self.prepare_mode_viewport(&mut producer, &request) {
            #[cfg(feature = "native-close-stage-diagnostic")]
            eprintln!(
                "native_body_phase_failure phase=prepare_mode cause={:?}",
                std::mem::discriminant(&cause)
            );
            return producer::merge_preparation_cleanup(Err(cause), producer.retire());
        }
        // Same-Source selection may change actual marker size; return old fonts/readers and recompose
        // instead of putting a new Input ticket on an old sealed frame.
        let refreshed = match producer.refresh_semantic_selection(self, &request) {
            Ok(refreshed) => refreshed,
            Err(cause) => {
                #[cfg(feature = "native-close-stage-diagnostic")]
                eprintln!(
                    "native_body_phase_failure phase=refresh_selection cause={:?}",
                    std::mem::discriminant(&cause)
                );
                return producer::merge_preparation_cleanup(Err(cause), producer.retire());
            } // Preserve both recomposition and cleanup failures; ordinary Drop alone could lose font/Source retirement failures.
        };
        if !refreshed {
            *self.producer.borrow_mut() = Some(producer);
            return Ok(()); // The font completion package still belongs to this sole Producer; resume only after its actual notification.
        }
        match self.advance_pending_cell(&mut producer, &request) {
            Ok(true) => {
                *self.producer.borrow_mut() = Some(producer);
                return Ok(());
            }
            Ok(false) => {}
            Err(cause) => {
                return producer::merge_preparation_cleanup(Err(cause), producer.retire());
            }
        }
        // Bind committed Find once after the sole normal Producer selects its original Source, without
        // borrowing an old Ready ticket,
        // capturing Source at begin, or waiting for an unrelated first frame before advancing the
        // target again.
        let binding = self
            .bind_pending_find(&producer, &request)
            .and_then(|()| self.bind_selection_reveal(&producer, &request));
        if let Err(cause) = binding {
            return producer::merge_preparation_cleanup(Err(cause), producer.retire());
        } // The local Producer must still consume delivered font results on binding failure; a bare ? cannot discard it.

        // Run the Producer outside its slot borrow; font, Native, and current callbacks may reenter but
        // observe actual preparation ownership.
        // Word deletion advances one bounded quantum of the original Composer without another layout,
        // font, or reader pipeline.
        let deleting_word = self.pending_word_delete.borrow().is_some()
            && !self.native_waits_for_vertical()
            && !self.native_front_needs_scene();
        let result = if deleting_word {
            self.advance_word_delete(&mut producer, &request)
        } else {
            producer.advance(self, &request)
        };
        #[cfg(feature = "native-close-stage-diagnostic")]
        if let Err(cause) = &result {
            // Identify only the original returned turn; PreparationRevoked still follows its ordinary
            // retirement path.
            eprintln!(
                "native_body_phase_failure phase=producer_advance cause={:?}",
                std::mem::discriminant(cause)
            );
        }
        let replaced = match producer.replaced_cell_source(&self.input, &request) {
            Ok(replaced) => replaced,
            Err(cause) => {
                // Source revalidation failure first retires the local Producer and preserves both
                // primary and cleanup errors instead of dropping its result via ?.
                return producer::merge_preparation_cleanup(Err(cause), producer.retire());
            }
        };
        if replaced
            || matches!(
                result,
                Err(NativeMarkdownPagedBodyError::PreparationRevoked)
            )
        {
            // Only original foreground revocation may yield to an actual successor; the Producer
            // already checked the original Source failure. Cancel and return
            // the complete old candidate; old worker completions retain their serial and cannot
            // authorize the new Source.
            producer.retire()?;
            self.clear_scene()?;
            self.serial.set(
                self.serial
                    .get()
                    .checked_add(1)
                    .ok_or(NativeMarkdownPagedBodyError::CounterExhausted)?,
            );
            self.revoked_preparations.set(
                self.revoked_preparations
                    .get()
                    .checked_add(1)
                    .ok_or(NativeMarkdownPagedBodyError::CounterExhausted)?,
            );
            self.schedule()?;
            return Ok(());
        }
        *self.producer.borrow_mut() = Some(producer);
        result?;
        if deleting_word {
            return self.finish_word_delete(); // The original Box is back in its slot; only Ready permits returning old reads and consuming the sole input permit.
        }
        self.finish_vertical_navigation()?;
        if self.finish_mode_viewport().inspect_err(|cause| {
            #[cfg(feature = "native-close-stage-diagnostic")]
            eprintln!(
                "native_body_phase_failure phase=finish_mode cause={:?}",
                std::mem::discriminant(cause)
            );
            let _ = cause;
        })? {
            return Ok(()); // Measured glyph restoration changed the clip; the old Scene ends here and the next original continuation certifies the new frame.
        }
        self.finish_source_target()?;
        self.finish_pending_find()?;
        if self.native_front_needs_scene() && self.scene.borrow().is_some() {
            self.schedule()?; // Wake uninterpreted events only after actual Scene publication, without polling while font or I/O work is in flight.
        }
        Ok(())
    }

    /// Save, mode transfer, and close must reject while actual input, layout, or Native geometry stacks
    /// remain active, preserving their current owner.
    pub(in crate::native_surface) fn preflight_release(&self) -> Result<()> {
        let child = self.active_cell_body();
        if let Some(child) = child {
            child.preflight_release()?;
        }
        if self.preparing.get()
            || self
                .view
                .borrow()
                .as_ref()
                .is_some_and(|view| view.is_busy())
            || self
                .viewport
                .borrow()
                .as_ref()
                .is_some_and(|viewport| viewport.is_busy())
            || self
                .gutter
                .borrow()
                .as_ref()
                .is_some_and(|gutter| gutter.is_busy())
        {
            return Err(NativeMarkdownPagedBodyError::Busy);
        }
        Ok(())
    }

    /// After accepting graph detachment, retain one completion receipt; actual pixel-release events
    /// alone permit final release verification.
    pub fn begin_close(&self) -> Result<Rc<NativeMarkdownPagedCloseReceipt>> {
        if let Some(receipt) = self.close_receipt.borrow().clone() {
            return Ok(receipt); // Repeated close returns the original metadata without joining again or cancelling successor business work.
        }
        self.preflight_release()?; // An active synchronous resource transaction retains the complete original document, Native graph, and Find.
        let view = self.view.borrow().clone();
        let receipt = NativeMarkdownPagedCloseReceipt::new(self.mtm);
        *self.close_receipt.borrow_mut() = Some(Rc::clone(&receipt));
        if let Some(pixels) = view.as_ref().and_then(|view| view.pixel_observer()) {
            receipt.track_pixels(&pixels.0);
        } // Track original pixels immediately on acceptance; a later child rejection cannot let the failed receipt claim early physical return.
        if let Err(error) = self.cancel_pending_find() {
            receipt.record(NativeMarkdownPagedCloseFailure::Body(error));
        }
        if let Err(error) = self.cancel_word_delete() {
            receipt.record(NativeMarkdownPagedCloseFailure::Body(error));
        }
        self.active.set(false);
        let pending_mode = self.pending_mode_action.borrow_mut().take();
        drop(pending_mode); // Accepted close revokes original business actions; queued weak identities cannot extend body or closing-resource lifetimes.
        let find_binding = self.find_highlights.borrow_mut().take();
        drop(find_binding);
        drop(self.pending_cell.borrow_mut().take());
        let active_cell = self.active_cell.borrow_mut().take();
        if let Some(active_cell) = active_cell {
            match active_cell.body.begin_close() {
                Ok(child_close) => receipt.follow_view(child_close),
                Err(cause) => receipt.record(NativeMarkdownPagedCloseFailure::Body(cause)),
            }
            active_cell.scroll.removeFromSuperview();
        }
        if let Some(cell_close) = self.cell_close.borrow_mut().take() {
            receipt.follow_view(cell_close);
        } // Follow only the previous finite pixel receipt still releasing, without retired-Cell lists, Source, or business documents.
        drop(self.mode_viewport.borrow_mut().take());
        drop(self.selection_reveal.borrow_mut().take());
        drop(self.pending_vertical.borrow_mut().take());
        drop(self.pending_page_scroll.borrow_mut().take());
        self.revoke_source_target();
        self.scheduled.set(None);
        self.deferred.set(false);
        self.font_notices.borrow_mut().clear();
        drop(self.paint_observation.borrow_mut().take());
        #[cfg(feature = "native-markdown-runtime-probe")]
        drop(self.preparation_probe.borrow_mut().take());
        self.remove_clip_observers();
        // First detach gutter Scene/fonts/pixels/client/ruler, then the body viewport; the same receipt
        // verifies actual Native and munmap facts.
        let gutter = self.gutter.borrow_mut().take();
        if let Some(gutter) = &gutter
            && let Err(error) = gutter.close(&receipt)
        {
            receipt.record(NativeMarkdownPagedCloseFailure::View(error));
        }
        drop(gutter);
        if let Err(cause) = self.clear_scene() {
            receipt.record(NativeMarkdownPagedCloseFailure::View(cause));
        }
        let producer = self.producer.borrow_mut().take();
        let producer_result = producer.map_or(Ok(()), Producer::retire);
        // End slot borrows and actually retire the sole continuation before join; earlier failure
        // cannot skip returning the worker and queued results.
        let worker_result = self
            .worker
            .borrow_mut()
            .take()
            .map_or(Ok(()), FontWorker::close);
        if let Err(error) = producer::merge_preparation_cleanup(producer_result, worker_result) {
            receipt.record(NativeMarkdownPagedCloseFailure::Body(error));
        }
        let viewport = self.viewport.borrow_mut().take();
        let viewport_result = viewport
            .as_ref()
            .map_or(Ok(()), |viewport| viewport.close());
        drop(viewport);
        if let Err(error) = viewport_result {
            receipt.record(NativeMarkdownPagedCloseFailure::Body(error.into()));
        }
        if let Some(view) = &view {
            match view.begin_close() {
                Ok(view_close) => receipt.follow_view(view_close),
                Err(error) => receipt.record(NativeMarkdownPagedCloseFailure::View(error)),
            }
        } // Child views transfer only atomic pixel facts and first failure; the parent receipt does not wait for caller-retained Views or a Cell parent document.
        drop(self.view.borrow_mut().take());
        drop(view);
        let input_result = self.input.close().map_err(ViewError::Input);
        if let Err(error) = input_result {
            receipt.record(NativeMarkdownPagedCloseFailure::View(error));
        }
        self.font_choices.borrow_mut().clear(); // Return fixed reuse slots and finite input first, retaining neither old Source nor font weak-control blocks.
        drop(self.typography.borrow_mut().take()); // Return the semantic NSFont policy with its active Body instead of retaining it in an idle global cache.
        let base_fonts = self.base_fonts.borrow_mut().take();
        drop(base_fonts); // Return both Native font-role claims synchronously so a closed Body or framework shell cannot retain them.
        let fonts = std::mem::take(&mut *self.fonts.borrow_mut());
        drop(fonts); // End every RefCell borrow before actual CF/Native font release can reenter.
        drop(self.document.borrow_mut().take());
        let cell = self.cell_session.borrow_mut().take();
        let cell_result = cell
            .as_ref()
            .map_or(Ok(()), |session| session.close().map_err(ViewError::Input));
        drop(cell); // Cancel and drain the original token after display/worker return; Input::close alone does not prove draft completion.
        if let Err(error) = cell_result {
            receipt.record(NativeMarkdownPagedCloseFailure::View(error));
        }
        // Mode switching retired a different actual Producer/worker/style-font set; final close must
        // verify more than the current set.
        // Read its original retirement receipt without retaining old resources or substituting current
        // font/Source closure for old-mode proof.
        if self.verify_mode_retirement().is_err() {
            receipt.record(NativeMarkdownPagedCloseFailure::Body(
                NativeMarkdownPagedBodyError::ResourceReleaseFailed,
            ));
        }
        receipt.seal(); // Synchronous business cleanup returned; deliver the original completion event only after all actual pixels are returned.
        Ok(receipt)
    }

    /// Every body Scene revocation also retires unpublished CPU candidates and weak gutter snapshots.
    /// Cell close may preserve the same Input/geometry. Retire the old candidate before successor Scene
    /// publication so its pending paint
    /// cannot mistake ordinary retirement between bounded turns for permanent input failure. Published
    /// pixels remain with the original CPU owner.
    fn clear_scene(&self) -> std::result::Result<(), ViewError> {
        let pending_link = self.pending_link.borrow_mut().take();
        let scene = self.scene.borrow_mut().take();
        // Clone only Rust Rc handles actually held by this Body, without Native retain, queries, or
        // Scene recapture.
        // End both state-slot and view-slot borrows before candidate/context destruction enters
        // reentrant Native code.
        let view = self.view.borrow().clone();
        if let Some(view) = view {
            view.retire_scene_candidate();
        }
        // Detach query work first; even scratch-close failure cannot skip subsequent gutter and Scene
        // return.
        // The caller preserves the first failure through its existing receipt/cleanup chain;
        // cancellation never swallows actual resource errors.
        let link_result = pending_link.map_or(Ok(()), link::PendingLink::retire);
        let gutter = self.gutter.borrow().clone();
        if let Some(gutter) = gutter {
            gutter.clear_snapshot();
        }
        drop(scene);
        link_result
    }

    /// An actual gutter callback failure belongs to the original Body failure; do not retry or certify
    /// a newer Scene.
    pub(in crate::native_surface) fn gutter_failed(&self, error: ViewError) {
        self.fail(error.into());
    }
}

impl NativeMarkdownPagedTextViewDelegate for NativeMarkdownPagedBody {
    /// Route the existing menu to the active Cell while preserving its draft selection and history.
    fn context_menu_client(
        &self,
    ) -> std::result::Result<Option<Rc<Retained<NativeMarkdownPagedTextView>>>, ViewError> {
        if !self.current_body() {
            return Err(ViewError::Revoked);
        }
        self.active_cell_body()
            .map(|child| child.view().map_err(cell::cell_view_error))
            .transpose() // The active Cell retains its selection and draft history without committing the draft or borrowing the parent document undo stack.
    }

    /// Match this same session epoch and foreground Body authority.
    fn current(&self, epoch: u64) -> bool {
        epoch == self.configuration.epoch && self.current_body()
    }
    /// Report accepted detachment separately from physical resource release.
    fn close_accepted(&self) -> bool {
        // Install the sole close receipt only after preflight; revoke active authority before the View
        // closes.
        // Synchronous Native detachment may call updateLayer/resetCursorRects; resource failures still
        // settle through the same close receipt.
        self.close_receipt.borrow().is_some()
    }
    /// Cancel only old mode-position intent before original wheel propagation continues.
    fn user_scroll(&self, epoch: u64) -> std::result::Result<(), ViewError> {
        if !self.current(epoch) {
            return Err(ViewError::Revoked);
        }
        self.cancel_mode_viewport(); // Revoke only the old position, preserving selection and original superclass wheel delivery without another scroll command.
        Ok(())
    }
    /// Scroll the original pointer gesture under stable Input/Down authority, rechecking actual Source
    /// health.
    fn drag_autoscroll(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        point: objc2_foundation::NSPoint,
        intent: &Weak<()>,
    ) -> std::result::Result<bool, ViewError> {
        if !self.current_body() {
            return Err(ViewError::Revoked);
        }
        self.find_source_health().map_err(|cause| {
            self.fail(cause); // The original Source failure cannot become an ordinary boundary no-op through later viewport work.
            ViewError::Revoked
        })?;
        let view = self.view().map_err(cell::cell_view_error)?;
        // Scrolling itself changes geometry, so its old geometry_epoch cannot reject its own valid
        // post-state.
        // Stable authority remains the same Doc/Cell, complete InputState, and original Down; recheck
        // all three around every Native step.
        let current = || {
            self.current_body()
                && view.cell_pointer_is_current(intent)
                && view.drag_scroll_point_is_current(point, intent)
                && Weak::ptr_eq(&request.document, &self.input.document_identity())
                && self
                    .input
                    .target_domain()
                    .is_ok_and(|target| target.same(&request.target))
                && self.input.state().is_ok_and(|state| state == request.input)
                && view.cell_pointer_is_current(intent)
                && view.drag_scroll_point_is_current(point, intent)
        };
        if !current() || !view.capture_scene_request()?.same(request) || !current() {
            return Err(ViewError::Revoked);
        }
        if self.has_pending_pointer_word(intent) {
            return Err(ViewError::Pending); // Pure word-selection gestures first return their original result instead of scrolling and revoking their own initial query.
        }
        self.cancel_mode_viewport(); // An actual gesture revokes the old mode anchor so it cannot pull the user back.
        let viewport = self.viewport.borrow().clone().ok_or(ViewError::Pending)?;
        let result = viewport.scroll_for_drag(point, &current);
        self.find_source_health().map_err(|cause| {
            self.fail(cause); // Preserve the actual Source failure even if Native reentry simultaneously revokes foreground authority.
            ViewError::Revoked
        })?;
        if !current() {
            return Err(ViewError::Revoked);
        }
        result.map_err(|_| ViewError::Revoked)
    }
    /// Acquire current bounded body geometry without extending an old geometry ticket.
    fn virtual_viewport(
        &self,
        epoch: u64,
        _bounds_width: f64,
    ) -> std::result::Result<NativeMarkdownPagedVirtualViewport, ViewError> {
        if !self.current(epoch) {
            return Err(ViewError::Revoked);
        }
        let viewport = self.viewport.borrow().clone().ok_or(ViewError::Pending)?;
        // This API acquires current geometry without an old caller ticket. During synchronous reentry,
        // await actual new
        // geometry without treating old pixels or revoked rectangles as Ready, or ordinary waiting as
        // permanent input failure.
        // Other failures retain original revocation semantics; consuming signed Scene/Geometry still
        // requires strict original-current checks.
        let geometry = viewport.geometry().map_err(|cause| match cause {
            super::paged_viewport::NativeMarkdownPagedViewportError::Busy => ViewError::Pending,
            _ => ViewError::Revoked,
        })?;
        // Body coordinates start at the frame excluding actual ruler/header occlusion; only the scroll
        // owner interprets the raw exposed clip.
        Ok(NativeMarkdownPagedVirtualViewport {
            document_origin: geometry.body_frame.origin,
            layout_origin_x: geometry.layout_origin_x,
            layout_width: geometry.layout_width,
            scroll_epoch: geometry.scroll_epoch,
        })
    }
    /// Return only a matching current Scene or request the sole existing Producer and report Pending.
    fn ready_scene(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
    ) -> std::result::Result<Rc<dyn NativeMarkdownPagedTextScene>, ViewError> {
        if !self
            .input
            .target_domain()
            .is_ok_and(|target| target.same(&request.target))
            || !Weak::ptr_eq(&request.document, &self.input.document_identity())
            || !self
                .input
                .state()
                .is_ok_and(|actual| actual == request.input)
        {
            return Err(ViewError::Revoked);
        }
        let scene = self.scene.borrow().clone();
        if let Some(scene) = scene
            && scene.key().request.same(request)
            && self.scene_current(scene.key())
        {
            return Ok(scene);
        }
        self.schedule().map_err(|_| ViewError::Revoked)?;
        Err(ViewError::Pending) // The same original Producer is queued; this Native event is neither retained nor replayed.
    }
    /// Certify the same published Scene, complete Input, and actual scroll/body origin.
    fn scene_current(&self, key: &NativeMarkdownPagedSceneKey) -> bool {
        let scene = self.scene.borrow().clone();
        let viewport = self.viewport.borrow().clone();
        self.current_body()
            && scene.is_some_and(|scene| scene.key().same(key))
            && self
                .input
                .state()
                .is_ok_and(|state| state == key.request.input)
            && viewport.is_some_and(|viewport| {
                viewport.geometry().is_ok_and(|geometry| {
                    geometry.is_current()
                        && geometry.scroll_epoch == key.request.virtual_viewport.scroll_epoch
                        // Match the actual body origin used to create the Scene; nonzero top margins
                        // cannot borrow the raw clip origin.
                        && geometry.body_frame.origin
                            == key.request.virtual_viewport.document_origin
                })
            })
    }
    /// Retire old display after the canonical commit, then notify weak consumers of that same receipt.
    fn committed(
        &self,
        commit: &NativeMarkdownPagedInputCommit,
    ) -> std::result::Result<(), ViewError> {
        commit.check_current(&|epoch| self.current(epoch))?;
        self.cancel_pending_mode_action(); // Real text or selection commits revoke old mode clicks without waiting for successor Ready to renew them.
        if !self.owns_word_commit(commit)
            && let Err(cause) = self.cancel_word_delete()
        {
            self.fail(cause); // A successor edit cannot turn the original Word Source failure into ordinary cancellation.
            return Err(ViewError::Revoked);
        }
        // Canonical text already committed; revoke old display and schedule the same continuation
        // without implying the transaction never happened.
        drop(self.selection_reveal.borrow_mut().take());
        drop(self.pending_vertical.borrow_mut().take());
        drop(self.pending_page_scroll.borrow_mut().take());
        self.clear_scene()?;
        self.schedule().map_err(|_| ViewError::Revoked)?;
        commit.check_current(&|epoch| self.current(epoch))?;
        let observers = self.input_observers.borrow().clone();
        for observer in observers
            .into_iter()
            .filter_map(|observer| observer.upgrade())
        {
            observer.committed(commit); // Borrow the original receipt for this synchronous stack; reentrant consumers must still validate it instead of acquiring newer Input authority.
            commit.check_current(&|epoch| self.current(epoch))?;
        }
        if self.input.is_cell() {
            let parent = self.cell_parent.borrow().upgrade();
            if let Some(parent) = parent {
                parent.notify_local_cell_input();
            }
        }
        Ok(())
    }
    /// Pass the committed receipt to the existing active-endpoint reveal path.
    fn reveal_committed_selection(
        &self,
        commit: &NativeMarkdownPagedInputCommit,
    ) -> std::result::Result<(), ViewError> {
        self.accept_selection_reveal(commit)
    }
    /// Activate the inserted table from its actual commit rather than recapturing selection.
    fn activate_inserted_table(
        &self,
        commit: &NativeMarkdownPagedInputCommit,
    ) -> std::result::Result<(), ViewError> {
        self.accept_inserted_table(commit)
            .map_err(cell::cell_view_error)
    }
    /// Admit the original vertical-navigation request; its existing continuation publishes the result.
    fn navigate_vertical(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        action: super::paged_text_view::NativeMarkdownPagedNavigation,
        extend: bool,
    ) -> std::result::Result<bool, ViewError> {
        self.accept_vertical_navigation(request, action, extend)?;
        Ok(true)
    }
    /// Admit bounded word deletion and retain actual preparation failures at the Body boundary.
    fn delete_word(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        forward: bool,
        permit: super::paged_input::NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<(), ViewError> {
        match self.accept_word_delete(request, forward, permit) {
            Ok(()) => Ok(()), // Acceptance only; without Ready, do not claim deletion occurred.
            Err(NativeMarkdownPagedBodyError::View(cause)) => Err(cause),
            Err(cause) => {
                self.fail(cause); // Retain actual Source/preparation failure instead of reporting successful deletion.
                Err(ViewError::Revoked)
            }
        }
    }
    /// Use original visual-motion authority; a non-View failure becomes the Body's sticky cause.
    fn move_visual(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        right: bool,
        extend: bool,
        permit: super::paged_input::NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<bool, ViewError> {
        match self.accept_visual_move(request, right, extend, permit) {
            Ok(()) => Ok(true),
            Err(NativeMarkdownPagedBodyError::View(cause)) => Err(cause),
            Err(cause) => {
                self.fail(cause);
                Err(ViewError::Revoked)
            }
        }
    }
    /// Admit one original word-motion request without claiming its boundary is already Ready.
    fn move_word(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        motion: super::paged_text_view::NativeMarkdownPagedWordMotion,
        extend: bool,
        permit: super::paged_input::NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<(), ViewError> {
        match self.accept_word_move(request, motion, extend, permit) {
            Ok(()) => Ok(()), // Admit one system action; publish selection only when the original bidi/group boundary becomes Ready.
            Err(NativeMarkdownPagedBodyError::View(cause)) => Err(cause),
            Err(cause) => {
                self.fail(cause); // Actual Source/resource/scheduling failures retain the Body cause; nonuniform input rejects its individual queued operation.
                Err(ViewError::Revoked)
            }
        }
    }
    /// Keep word selection on the original Source and permit until its query actually completes.
    fn select_word(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        selection: super::paged_text_view::NativeMarkdownPagedWordSelection,
        gesture: Option<Weak<()>>,
        permit: super::paged_input::NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<(), ViewError> {
        match self.accept_word_selection(request, selection, gesture, permit) {
            Ok(()) => Ok(()), // Admit the actual word query; only original-Source Ready and the same permit trigger selection completion.
            Err(NativeMarkdownPagedBodyError::View(cause)) => Err(cause),
            Err(cause) => {
                self.fail(cause); // Source/index/resource failure remains with the Body and cannot masquerade as successful word selection.
                Err(ViewError::Revoked)
            }
        }
    }
    /// Preserve arrival order for an actual Native key without letting an old mode target precede it.
    fn defer_key_down(
        &self,
        event: &objc2_app_kit::NSEvent,
        permit: &super::paged_input::NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<bool, ViewError> {
        self.cancel_mode_viewport(); // Even while a real key waits for its caret in the original queue, an old mode target must not pull back the viewport first.
        match self.accept_native_key(event, permit) {
            Ok(accepted) => Ok(accepted),
            Err(NativeMarkdownPagedBodyError::View(cause)) => Err(cause),
            Err(cause) => {
                self.fail(cause);
                Err(ViewError::Revoked)
            }
        } // The Body retains actual Source/resource failure; quota rejection preserves already-admitted events.
    }
    /// Expose the actual existing input barrier, not pixel-preparation activity.
    fn has_native_input_barrier(&self) -> bool {
        self.native_input_pending()
    }
    /// Queue formatting in the same original Native input chain and retain typed failure ownership.
    fn defer_format(
        &self,
        action: omni_markdown_core::MarkdownFormatAction,
        permit: &super::paged_input::NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<bool, ViewError> {
        match self.accept_native_edit(word::native::NativeInput::Format(action), permit) {
            Ok(accepted) => Ok(accepted),
            Err(NativeMarkdownPagedBodyError::View(cause)) => Err(cause),
            Err(cause) => {
                self.fail(cause);
                Err(ViewError::Revoked)
            }
        }
    }
    /// Queue table insertion at its actual input position without precomputing stale content.
    fn defer_table_insertion(
        &self,
        permit: &super::paged_input::NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<bool, ViewError> {
        match self.accept_native_edit(word::native::NativeInput::InsertTable, permit) {
            Ok(accepted) => Ok(accepted),
            Err(NativeMarkdownPagedBodyError::View(cause)) => Err(cause),
            Err(cause) => {
                self.fail(cause);
                Err(ViewError::Revoked)
            }
        }
    }
    /// Order Source folding with the same Native input chain rather than changing folds early.
    fn defer_source_fold(
        &self,
        action: super::paged_text_view::NativeMarkdownPagedSourceFoldAction,
        permit: &super::paged_input::NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<bool, ViewError> {
        match self.accept_native_edit(word::native::NativeInput::Fold(action), permit) {
            Ok(accepted) => Ok(accepted),
            Err(NativeMarkdownPagedBodyError::View(cause)) => Err(cause),
            Err(cause) => {
                self.fail(cause);
                Err(ViewError::Revoked)
            }
        }
    }
    /// Admit the original menu selector while preserving key ordering and captured clipboard payload.
    fn defer_native_command(
        &self,
        selector: objc2::runtime::Sel,
        permit: &super::paged_input::NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<bool, ViewError> {
        match self.accept_native_command(selector, permit) {
            Ok(accepted) => Ok(accepted),
            Err(NativeMarkdownPagedBodyError::View(cause)) => Err(cause),
            Err(cause) => {
                self.fail(cause);
                Err(ViewError::Revoked)
            }
        } // Menus retain original key ordering without rereading a changed clipboard payload later.
    }
    /// Keep attachment paths with the original input chain until its queue head owns copying.
    fn defer_attachments(
        &self,
        paths: Vec<std::path::PathBuf>,
        drop: Option<(u64, usize)>,
        permit: &super::paged_input::NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<bool, ViewError> {
        // The original Word chain owns attachments; copying occurs only at queue head, while
        // close/revocation synchronously returns unprocessed paths.
        match self.accept_native_edit(
            word::native::NativeInput::Attachments { paths, drop },
            permit,
        ) {
            Ok(accepted) => Ok(accepted),
            Err(NativeMarkdownPagedBodyError::View(cause)) => Err(cause),
            Err(cause) => {
                self.fail(cause);
                Err(ViewError::Revoked)
            }
        }
    }
    /// Retain rejection feedback in the existing ordered attachment chain.
    fn defer_attachment_rejection(&self, message: &str) -> bool {
        self.retain_attachment_rejection(message)
    }
    /// Bind the actual interpreted before-state and Rc receipt to their original input owner.
    fn begin_interpreted_commit(
        &self,
        before: super::paged_input::NativeMarkdownPagedInputState,
        commit: Rc<NativeMarkdownPagedInputCommit>,
    ) -> std::result::Result<bool, ViewError> {
        match self.begin_native_commit(before, commit) {
            Ok(accepted) => Ok(accepted),
            Err(NativeMarkdownPagedBodyError::View(cause)) => Err(cause),
            Err(cause) => {
                self.fail(cause);
                Err(ViewError::Revoked)
            }
        } // Certify this interpretation callback's actual before/Commit only; global depth cannot claim an external commit.
    }
    /// Complete effects for the already-published receipt without replaying its Core transaction.
    fn finish_interpreted_commit(
        &self,
        commit: &NativeMarkdownPagedInputCommit,
        reveal: bool,
    ) -> std::result::Result<(), ViewError> {
        match self.finish_native_commit(commit, reveal) {
            Ok(()) => Ok(()),
            Err(NativeMarkdownPagedBodyError::View(cause)) => Err(cause),
            Err(cause) => {
                self.fail(cause);
                Err(ViewError::Revoked)
            }
        } // The original Commit published; retain the actual effect failure for the View to wrap as CommittedEffect.
    }
    /// Queue document endpoint intent; later receipts supply its actual length and anchor.
    fn defer_document_motion(
        &self,
        end: bool,
        extend: bool,
        permit: &super::paged_input::NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<bool, ViewError> {
        match self.accept_document_motion(end, extend, permit) {
            Ok(accepted) => Ok(accepted), // Admit original endpoint intent; future length/anchor comes only from the preceding actual receipt.
            Err(NativeMarkdownPagedBodyError::View(cause)) => Err(cause),
            Err(cause) => {
                self.fail(cause); // Retain actual Source/scheduling failure rather than claiming the endpoint was reached.
                Err(ViewError::Revoked)
            }
        }
    }
    /// Queue editing semantics while the existing Core path owns indentation, list continuation, and
    /// history.
    fn defer_source_command(
        &self,
        command: omni_markdown_core::MarkdownEditingCommand,
        permit: &super::paged_input::NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<bool, ViewError> {
        match self.accept_source_command(command, permit) {
            Ok(accepted) => Ok(accepted), // Admission fixes intent only; later original Core results still determine list continuation, indentation, and history.
            Err(NativeMarkdownPagedBodyError::View(cause)) => Err(cause),
            Err(cause) => {
                self.fail(cause); // Retain actual Source/resource/scheduling failure without reporting command execution.
                Err(ViewError::Revoked)
            }
        }
    }
    /// Keep grapheme deletion ordered until the sole Producer certifies its real boundaries.
    fn defer_grapheme_delete(
        &self,
        forward: bool,
        permit: &super::paged_input::NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<bool, ViewError> {
        match self.accept_grapheme_delete(forward, permit) {
            Ok(accepted) => Ok(accepted), // Ordered admission only; the sole original Producer still owns the actual range and document transaction.
            Err(NativeMarkdownPagedBodyError::View(cause)) => Err(cause),
            Err(cause) => {
                self.fail(cause); // Retain original Source/resource/scheduling failure without reporting successful deletion.
                Err(ViewError::Revoked)
            }
        }
    }
    /// Admit only a bounded original insertion payload; synchronous input remains unchanged when not
    /// deferred.
    fn defer_insert(
        &self,
        text: &objc2::runtime::AnyObject,
        permit: &super::paged_input::NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<bool, ViewError> {
        match self.accept_word_typing(text, permit) {
            Ok(accepted) => Ok(accepted), // False keeps the original synchronous path; true means only that the bounded payload entered its original chain in order.
            Err(NativeMarkdownPagedBodyError::View(cause)) => Err(cause),
            Err(cause) => {
                self.fail(cause); // Retain actual Source/scheduling failure instead of claiming characters committed.
                Err(ViewError::Revoked)
            }
        }
    }
    /// Publish the original Source selection through its existing bounded independent output owner.
    fn copy_selection(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
    ) -> std::result::Result<(), ViewError> {
        self.copy_source_selection(request)
    }
    /// Use the original page-scroll request and retain actual Source/resource failure in this Body.
    fn scroll_page(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        down: bool,
    ) -> std::result::Result<(), ViewError> {
        match self.accept_page_scroll(request, down) {
            Ok(()) => Ok(()),
            Err(NativeMarkdownPagedBodyError::View(cause)) => Err(cause),
            Err(cause) => {
                self.fail(cause); // Retain the Source/resource failure in the Body; unknown internal failure cannot masquerade as completed scrolling.
                Err(ViewError::Revoked)
            }
        }
    }
    /// Route child actions through their actual parent; a standalone Cell only owns cancellation.
    fn cell_action(
        &self,
        action: super::paged_text_view::NativeMarkdownPagedCellAction,
        permit: super::paged_input::NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<(), ViewError> {
        let parent = self.cell_parent.borrow().upgrade();
        if let Some(parent) = parent {
            return parent.accept_cell_action(self, action, permit);
        }
        match action {
            super::paged_text_view::NativeMarkdownPagedCellAction::Cancel => {
                self.cancel_cell(permit)
            }
            _ => Err(ViewError::Pending), // The original table controller owns RequiredRow navigation; Tab/Return must not enter Cell body text.
        }
    }

    /// Offer actual marked-text mouse input to the live child without reopening an ended draft.
    fn handle_cell_marked_mouse(
        &self,
        event: &objc2_app_kit::NSEvent,
    ) -> std::result::Result<bool, ViewError> {
        let child = self.active_cell_body();
        let Some(child) = child else {
            return Ok(false);
        };
        match child.input.state() {
            Err(super::paged_input::NativeMarkdownPagedInputError::Closed) => {
                // After Core seals while Native is still on-stack, actual subsequent Drag/Up belongs to
                // the parent gesture;
                // an ended draft cannot provide another IME origin. The original queued detach still
                // returns the physical child.
                if self
                    .pending_cell
                    .borrow()
                    .as_ref()
                    .is_some_and(|pending| matches!(&**pending, cell::PendingCell::Closing { .. }))
                {
                    return Err(ViewError::Pending); // The same-request Esc cleanup is still in flight; preserve original canonical authority.
                }
                return Ok(false);
            }
            Err(cause) => return Err(cause.into()), // Other Source/input failures cannot hide behind draft completion.
            Ok(_) => {}
        }
        child
            .view()
            .map_err(cell::cell_view_error)?
            .handle_marked_mouse(event)
    }

    /// Admit the original pointer request against its actual Cell and original input permit.
    fn cell_pointer(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        pointer: super::paged_text_view::NativeMarkdownPagedCellPointer,
        permit: super::paged_input::NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<bool, ViewError> {
        self.accept_cell_pointer(request, pointer, permit)
    }

    /// Leave pointer lifetime termination with the original View while pending endpoint selection may
    /// finish.
    fn finish_cell_pointer(
        &self,
        _request: &NativeMarkdownPagedSceneRequest,
    ) -> std::result::Result<(), ViewError> {
        Ok(()) // The original View slot immediately revokes Up authority; in-flight Ready completes endpoint selection without renewing pointer lifetime.
    }

    /// Extend selection through the original Cell pointer request and permit.
    fn extend_cell_pointer(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        extension: super::paged_text_view::NativeMarkdownPagedCellExtension,
        permit: super::paged_input::NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<bool, ViewError> {
        self.extend_cell_selection(request, extension, permit)
    }

    /// Queue the original finite pointer location without acquiring successor Input authority.
    fn defer_pointer(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        pointer: super::paged_text_view::NativeMarkdownPagedPointerLocation,
        permit: super::paged_input::NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<bool, ViewError> {
        self.defer_cell_pointer(request, pointer, permit)
    }

    /// Cancel only the existing deferred Cell pointer continuation.
    fn cancel_deferred_pointer(&self) -> bool {
        self.cancel_deferred_cell_pointer()
    }

    /// Transfer an actual child drag to its live original parent without reconstructing that parent.
    fn drag_cell_pointer_outside(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        pointer: super::paged_text_view::NativeMarkdownPagedPointerLocation,
        permit: super::paged_input::NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<bool, ViewError> {
        let Some(parent) = self.cell_parent.borrow().upgrade() else {
            return Ok(false);
        };
        parent.accept_cell_drag(self, request, pointer, permit)
    }
}

impl Drop for NativeMarkdownPagedBody {
    fn drop(&mut self) {
        // Normal business explicitly calls begin_close and verifies its receipt; early return only
        // performs bounded graph detachment without reconstruction.
        let _ = self.begin_close(); // Drop accepts cleanup without treating an unsubscribed/unverified receipt as release success.
    }
}
