//! Source正文的正式分页owner，只消费同一canonical和有界视口管线。
//!
//! 没有TextKit、整篇字符串或第二文档；原生预览grid等尚未迁移的能力仍明确拒绝，
//! 不能把Source接入或局部绘图结果当作完整产品的功能与内存验收。

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
    /// 原Core错误是完整的typed事实而非std::error::Error；保留枚举供诊断，界面只显示上方中文。
    fn from(
        error: omni_markdown_core::markdown_document_buffer::MarkdownTableRequestError,
    ) -> Self {
        Self::Semantics(error)
    }
}

type Result<T> = std::result::Result<T, NativeMarkdownPagedBodyError>;

/// 装配只转交实际owner；两类输入共用下方唯一原生初始化图，不提供公开范围造Cell的入口。
enum Attachment {
    Body(Rc<RefCell<MarkdownDocumentBuffer>>),
    Cell(Rc<NativeMarkdownPagedCellSession>),
}

/// 颜色由原界面样式调用方提供；候选不另造主题，也不改变正文和选区的颜色语义。
#[derive(Clone, Copy)]
pub struct NativeMarkdownPagedBodyColors {
    pub background: [f64; 4],
    /// Source活动逻辑行的原界面底色；不在绘图阶段推导主题或调整正文颜色。
    pub active_line: [f64; 4],
    pub text: [f64; 4],
    pub selection: [f64; 4],
    pub caret: [f64; 4],
}

/// 全部字段均为有限配置；字号取实际 CTFont，不另按名字重建系统字体。
pub struct NativeMarkdownPagedBodyConfiguration {
    /// Source保持字面路径；InlinePreview候选消费已认证RootLeaf字体及显隐/下划线，不代表GeneralInline完整预览。
    pub presentation_mode: omni_markdown_core::MarkdownPresentationMode,
    pub epoch: u64,
    pub maximum_font_bytes: u64,
    pub padding: f64,
    pub colors: NativeMarkdownPagedBodyColors,
}

/// 基础字体是显式的 Native 资源用途；不预建第二套 Core palette 或排版续体。
/// 通用挂载严格沿调用方同一字体，产品编辑器才分别指定源码与预览的文稿字体。
enum BaseFonts {
    Single(CFRetained<CTFont>),
    Presentation {
        source: CFRetained<CTFont>,
        preview: CFRetained<CTFont>,
    },
}
impl BaseFonts {
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

/// 只读原 composer 的实际计数；缺少 Ready 或采样不能由这些字段推断产品验收通过。
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
    /// 原 step 的累计读取包含 Bidi/Script/GCB/LB 流式扫描；不是唯一字节数或 heap 容量。
    pub composer_step_bytes: usize,
    pub composer_step_scalars: usize,
    pub composer_step_metadata_units: usize,
    pub maximum_step_bytes: usize,
    pub maximum_step_scalars: usize,
    pub maximum_step_metadata_units: usize,
    pub trial_source_bytes: usize,
    /// 当前唯一语义上下文的原累计工作；独立于Composer计量，Source模式始终为零。
    pub semantic_work: omni_markdown_core::markdown_document_buffer::SemanticWork,
    pub source_total_bytes: usize,
    pub complete: bool,
    pub target_lookup_records: usize,
    pub target_metric_parts: usize,
    pub target_walk_lines: usize,
    pub target_index_records: usize,
    /// 仅远端批次尚未完成真实字体度量的行数；普通逐行准备不计入此目标工作量。
    pub target_unmeasured_lines: usize,
    /// 原队列末行的真实来源范围；只读既有 LineReady 事实，不触发排版或保留来源对象。
    pub target_unmeasured_last_source: Option<std::ops::Range<usize>>,
    pub target_max_batch_lines: usize,
    /// 只计原目标准备期间实际调用的checkpoint，不包含之前普通首屏/滚动调用。
    pub target_checkpoint_attempts: usize,
    /// 实际checkpoint入口累计耗时，含guard/sync/dup；不能解释为单独fsync的测量。
    pub target_checkpoint_millis: f64,
    /// 批次末行、前驱和part访问实际读取的64B记录数，不与旧前缀Y回溯混计。
    pub target_batch_index_records: usize,
}

/// 一个实际表格格的原资源观察者；只保弱身份和原子事实，不延长显示Source、目录FD或worker。
/// 尚未创建的资源为None；已经开始的Display转换也观察其真实writer，不漏掉未发布目录。
pub struct NativeMarkdownPagedCellResourceReceipts {
    /// 原显示转换准备中或已完成时观察其Source与坐标目录；不创建额外读者。
    pub display: Option<omni_markdown_core::MarkdownTableCellDisplayResourceObserver>,
    /// 仍有实际Composer时观察原writer；完成后已移交Reader的格不伪造一个writer。
    pub writer: Option<omni_markdown_core::MarkdownComposedLineResourceObserver>,
    /// 已封存可见前缀的实际Reader，旧Scene短期持有同一资源时此收据仍能观察其最终归还。
    pub reader: Option<omni_markdown_core::MarkdownComposedLineResourceObserver>,
    /// 原Composer的真实各角色观察；没有Composer的格不建立worker来满足收据形状。
    pub worker: Option<omni_markdown_core::MarkdownLineComposerWorkerResourceObserver>,
}

impl NativeMarkdownPagedCellResourceReceipts {
    /// 只核本次实际取得的格资源：Source载荷、投影FD/工作页、原排版目录和真实worker都须归还。
    /// 原存储错误和观察失败同样拒绝；None仅代表该阶段未持此角色，不能补签未观察的历史资源。
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
        // Reader与writer分别观察实际原目录；不以Cell对象消失或一个取消位代替FD与映射归还。
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
                // 原worker已包含各实际角色的FD/map/Source覆盖；不能在这里造第二张角色表。
                return Err(format!("原表格worker资源未归还：{actual:?}"));
            }
        }
        Ok(())
    }
}

/// 原构造资源的固定观察者；没有 Doc、Source、字体或文件强边。
pub struct NativeMarkdownPagedBodyResourceReceipts {
    pub source: omni_markdown_core::MarkdownLayoutTextSourceObserver,
    /// 尚未创建任何prefix Reader时为None；正在度量的原pending Reader同样返回真实观察者。
    pub reader: Option<omni_markdown_core::MarkdownComposedLineResourceObserver>,
    pub writer: omni_markdown_core::MarkdownComposedLineResourceObserver,
    pub worker: omni_markdown_core::MarkdownLineComposerWorkerResourceObserver,
    pub fonts: Vec<(
        omni_markdown_core::MarkdownLayoutFontBytesResourceObserver,
        omni_markdown_core::MarkdownLayoutFontResourceObserver,
    )>,
    /// 当前实际Grid、可见Grid及恢复中的格观察；不登记历史、不复制Source或新建读取者。
    pub cells: Vec<NativeMarkdownPagedCellResourceReceipts>,
}

/// 当前窗口只观察原Input已经发布的收据；通知不持正文，也不代表像素或磁盘保存已完成。
pub trait NativeMarkdownPagedBodyInputObserver {
    fn committed(&self, commit: &NativeMarkdownPagedInputCommit);
    /// 局部草稿的输入或客户切换只改变工具栏事实，不能伪装成canonical编辑而启动保存。
    fn local_input_changed(&self);
}

/// composition root 持唯一 Rc；View 的委托和异步延续都只保 Weak。
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
    // Rc 只让同步装配在槽借用外借用两份不可变 Native 字体；关闭会取走实际强 claim。
    base_fonts: RefCell<Option<Rc<BaseFonts>>>,
    // 只复用同真实Font与完整小GCB输入的已完成选择；弱字体票不延长原palette资源寿命。
    font_choices: RefCell<font_choice::FontChoices>,
    typography: RefCell<Option<semantics::Typography>>,
    catalog: MarkdownLineFontCatalog,
    // 同一父子图借原已捕获段落事实；活动格不会重新读取系统样式或复制字体文件。
    paragraph: Rc<super::paged_paragraph_style::NativeMarkdownLineParagraph>,
    active_cell: RefCell<Option<Rc<cell::ActiveCell>>>,
    cell_close: RefCell<Option<Rc<NativeMarkdownPagedCloseReceipt>>>,
    cell_parent: RefCell<Weak<Self>>,
    pending_cell: RefCell<Option<Rc<cell::PendingCell>>>,
    // 原续体只分配一次；每轮借出和放回移动唯一Box，避免反复搬运整份排版工作状态。
    producer: RefCell<Option<Box<Producer>>>,
    close_receipt: RefCell<Option<Rc<NativeMarkdownPagedCloseReceipt>>>,
    // 模式只保上一份实际退场的 Source/目录/worker/样式字体观察，不累积历史 Native 图。
    mode_retirement: RefCell<Option<NativeMarkdownPagedBodyResourceReceipts>>,
    worker: RefCell<Option<FontWorker>>,
    scene: RefCell<Option<Rc<Scene>>>,
    source_target: RefCell<Option<Rc<target::SourceTargetRequest>>>,
    mode_viewport: RefCell<Option<mode::PendingModeViewport>>,
    // 按钮点击只保这一份原弱意图；排版Pending时不辞任输入、不预先更改真实模式。
    pending_mode_action: RefCell<Option<Rc<mode::PendingModeAction>>>,
    // 真实按钮每次点击都撤旧意图；它不推进文稿或布局代次，也不是切换完成标记。
    mode_action_revision: Cell<u64>,
    selection_reveal: RefCell<Option<NativeMarkdownPagedSceneRequest>>,
    pending_vertical: RefCell<Option<Rc<navigation::PendingVertical>>>,
    pending_page_scroll: RefCell<Option<Rc<page_scroll::PendingPageScroll>>>,
    pending_find: RefCell<Option<Rc<find::PendingFind>>>,
    // 栏与controller都只弱绑定；Scene只保存当次有限匹配，不让查询或Native owner进入字形目录。
    find_highlights: RefCell<Option<find_highlights::Binding>>,
    pending_link: RefCell<Option<link::PendingLink>>,
    pending_word_delete: RefCell<Option<Rc<word::PendingWordDelete>>>,
    input_observers: RefCell<Vec<Weak<dyn NativeMarkdownPagedBodyInputObserver>>>,
    paint_observation: RefCell<Option<PaintObservation>>,
    #[cfg(feature = "native-markdown-runtime-probe")]
    preparation_probe: RefCell<Option<Weak<dyn NativeMarkdownPagedBodyPreparationProbe>>>,
    revoked_preparations: Cell<usize>,
    // 模式属于当前正文会话；启动配置只提供初值，切换不重建 Input、历史或折叠集合。
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
    /// 当前模式由同一个正文会话拥有；窗口和后继 Producer 都读取这一实际事实。
    pub(in crate::native_surface) fn presentation_mode(
        &self,
    ) -> omni_markdown_core::MarkdownPresentationMode {
        self.presentation.get()
    }

    /// 移交原文档 owner 并挂在原 NSScrollView；失败时局部候选沿原释放边归还。
    ///
    /// 调用方在业务关闭后仍须核实 Native Weak、映射和关闭收据；本方法只挂载传入滚动区，
    /// 展示模式由配置和同一 Body 的后续模式事务拥有，不新增窗口、快捷键或标题栏。
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

    /// 产品编辑器显式区分两种文稿用途；普通 attach 的单字体合同不因此改变。
    /// 初次模式只捕获对应的一份 Core 字体，另一用途只保留 Native 字体 claim。
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

    /// 真实Cell会话沿同一视口渲染；原Doc继续归外层model，不复制草稿或创建隐藏TextKit。
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

    fn attach_target(
        target: Attachment,
        scroll: &NSScrollView,
        base_fonts: Rc<BaseFonts>,
        paragraph: &NSParagraphStyle,
        configuration: NativeMarkdownPagedBodyConfiguration,
        mtm: MainThreadMarker,
    ) -> Result<Rc<Self>> {
        // 配置出错在创建 Native 图之前拒绝；非有限颜色不能被静默夹成别的主题。
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
            registered // Source 沿原 CT 行高/基线，不构造系统 paragraph metrics owner。
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

    /// 两类输入只在此建立原生图。活动格借父亲已经注册的实际字体与段落，
    /// 与公开独立 Cell 附着共用此处的视口、通知、输入和失败释放，不再复制一套挂载流程。
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
                } // 草稿局部坐标不借原 Doc 的 RootLeaf 语义。
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
            // 行号在挂正文前占用真实ruler宽度，Source/Cell的原padding不混入此布局。
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
        // 初始高度只使用真实 clip 的最小可见范围；没有按字数估计或声称这是整篇高度。
        let viewport = NativeMarkdownPagedViewport::attach(scroll, &view, frame.size.height, mtm)?;
        *owner.viewport.borrow_mut() = Some(viewport);
        owner.observe_clip(&scroll.contentView());
        owner.schedule()?;
        Ok(owner)
    }

    /// 临时取得实际正文视图；调用方不能把它保存在关闭完成票中延长业务资源。
    pub fn view(&self) -> Result<Rc<Retained<NativeMarkdownPagedTextView>>> {
        self.view
            .borrow()
            .clone()
            .ok_or(NativeMarkdownPagedBodyError::Closed)
    }

    /// 正式保存、历史状态与输入观察共用真实提交出口；追加弱订阅不能替换已有业务消费者。
    pub fn observe_input(
        &self,
        observer: Weak<dyn NativeMarkdownPagedBodyInputObserver>,
    ) -> Result<()> {
        if !self.current_body() {
            return Err(NativeMarkdownPagedBodyError::Closed);
        }
        let mut observers = self.input_observers.borrow_mut();
        // 已退出的输入会话不留目录项；只保真实仍活着的消费者，不缓存提交或业务正文。
        observers.retain(|registered| registered.strong_count() != 0);
        if !observers
            .iter()
            .any(|registered| Weak::ptr_eq(registered, &observer))
        {
            observers.push(observer);
        }
        Ok(())
    }

    /// 保存或模式交接不能越过已接纳、尚未发布的键盘动作；普通像素准备不冒充未完成编辑。
    pub(in crate::native_surface) fn has_pending_input(&self) -> bool {
        self.pending_word_delete.borrow().is_some() || self.pending_vertical.borrow().is_some()
    }

    /// 新挂载的实际模式以当前完整输入票定位；不为定位伪造选择提交或增加Undo。
    pub(in crate::native_surface) fn reveal_current_selection(&self) -> Result<()> {
        let request = self.view()?.capture_scene_request()?;
        if !self.current_body() || !self.input.state().is_ok_and(|state| state == request.input) {
            return Err(ViewError::Revoked.into());
        }
        *self.selection_reveal.borrow_mut() = Some(request);
        self.schedule()
    }

    pub fn take_failure(&self) -> Option<NativeMarkdownPagedBodyError> {
        self.failure.borrow_mut().take()
    }

    /// 目标用途先消费原票再捕获；有限活字体只用于真实实例/冻结映射复用，不持旧 Scene。
    /// Native 调用前已结束所有 Body 槽借用，失败由原调用方保留 Source 与资源首错。
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

    /// 只有实际已封存前缀的高度；它不代表尚未排版的文档尾部。
    pub fn prepared_height(&self) -> Option<f64> {
        self.producer
            .borrow()
            .as_ref()
            .map(|producer| producer.height())
    }

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

    pub fn is_preparing(&self) -> bool {
        self.scheduled.get().is_some()
            || self.preparing.get()
            || self
                .producer
                .borrow()
                .as_ref()
                .is_some_and(|producer| producer.has_in_flight())
    }

    fn current_body(&self) -> bool {
        self.active.get()
            && !self.failed.get()
            && !self
                .off_main_notice
                .load(std::sync::atomic::Ordering::Acquire)
    }

    fn schedule(&self) -> Result<()> {
        if !self.current_body() {
            return Err(NativeMarkdownPagedBodyError::Closed);
        }
        let serial = self.serial.get();
        if self.scheduled.get() == Some(serial) {
            return Ok(());
        }
        self.scheduled.set(Some(serial));
        let weak = MainThreadBound::new(self.weak.clone(), self.mtm);
        super::enqueue_native_markdown_main_task(move || {
            let mtm = MainThreadMarker::new().expect("正文延续只在 AppKit 主队列执行");
            let Some(owner) = weak.get(mtm).upgrade() else {
                return;
            };
            // 旧请求不能清理后继 scheduled 槽，也不能领取后继文档重新执行旧工作。
            if owner.serial.get() != serial
                || owner.scheduled.get() != Some(serial)
                || !owner.active.get()
            {
                return;
            }
            owner.scheduled.set(None);
            if owner.word_input_is_copying() {
                return; // 这次真实载荷getter尚未归还；完成出口会恢复同一调度，不能排空转任务。
            }
            if owner.preparing.replace(true) {
                owner.deferred.set(true);
                return;
            }
            let result = owner.advance();
            owner.preparing.set(false);
            if let Err(cause) = result {
                owner.fail(cause);
                return;
            }
            if let Err(cause) = owner.finish_cell_work() {
                owner.fail(cause);
                return;
            } // 父子同步prepare已退出，才消费真正Ready的格动作与唯一canonical提交。
            if owner.font_notice_failed.replace(false) {
                owner.fail(NativeMarkdownPagedBodyError::WorkerFailed);
                return;
            }
            // 只交付实际已经到达的有限完成事件，不轮询 receiver 或等待 Weak 变空。
            let notices = std::mem::take(&mut *owner.font_notices.borrow_mut());
            for serial in notices {
                if let Err(cause) = owner.finish_font_trial(serial) {
                    owner.fail(cause);
                    return;
                }
            }
            if owner.deferred.replace(false)
                && !owner.word_input_is_copying()
                && let Err(cause) = owner.schedule()
            {
                owner.fail(cause);
            }
            // Producer真实发布只登记原key；这里所有同步准备与槽借用均已归还，才交模式动作一次。
            owner.deliver_pending_mode_action();
        });
        Ok(())
    }

    fn fail(&self, cause: NativeMarkdownPagedBodyError) {
        self.fail_pending_find(&cause);
        // 首错是 sticky：停止原续体并清掉 Ready，不用再次捕获较新 Source 隐藏错误。
        if self.failure.borrow().is_none() {
            #[cfg(feature = "native-markdown-runtime-probe")]
            {
                // 只在原首次失败边界记录枚举分类；后继Busy/Revoked不能替代原准备首因。
                // 只读这一层已持有的typed cause，不遍历错误链、不展开String/Source或正文路径。
                // View错误可能持有链接或附件错误文本，因此不对整个ViewError做Debug。
                let view = match &cause {
                    NativeMarkdownPagedBodyError::View(error) => {
                        Some(std::mem::discriminant(error))
                    }
                    _ => None,
                };
                // Core分类区分权限、覆盖、断行和整形；具体Source与分页资源仍留在原错误中。
                let compose = match &cause {
                    NativeMarkdownPagedBodyError::Compose(error) => {
                        Some(std::mem::discriminant(error))
                    }
                    _ => None,
                };
                // 字体捕获可携带系统或文件失败信息；这里只识别捕获类型，不打印描述或URL。
                let font = match &cause {
                    NativeMarkdownPagedBodyError::Font(error) => {
                        Some(std::mem::discriminant(error))
                    }
                    _ => None,
                };
                // 注册失败属于原真实catalog，不为诊断重新解析、注册或获取字体权限。
                let registration = match &cause {
                    NativeMarkdownPagedBodyError::Registration(error) => {
                        Some(std::mem::discriminant(error))
                    }
                    _ => None,
                };
                // 已捕获的段落失败只读枚举；不在错误边界再次调用Native样式getter。
                let paragraph = match &cause {
                    NativeMarkdownPagedBodyError::Paragraph(error) => {
                        Some(std::mem::discriminant(error))
                    }
                    _ => None,
                };
                // 滚动几何失败与正文源失败分开标明，不重新捕Scene或领取后继窗口。
                let viewport = match &cause {
                    NativeMarkdownPagedBodyError::Viewport(error) => {
                        Some(std::mem::discriminant(error))
                    }
                    _ => None,
                };
                // 候选试次不因观察而继续推进；完整内层原因及资源归还义务仍归原cause。
                let fallback = match &cause {
                    NativeMarkdownPagedBodyError::Fallback(error) => {
                        Some(std::mem::discriminant(error))
                    }
                    _ => None,
                };
                // 语义层保留WorkLimit/撤销/Source分类，不输出其可能包含的冻结源错误载荷。
                let semantics = match &cause {
                    NativeMarkdownPagedBodyError::Semantics(error) => {
                        Some(std::mem::discriminant(error))
                    }
                    _ => None,
                };
                // 仅真实std::io::Error可投影errno；不格式化其可能包含外部载荷的message。
                let storage_errno = match &cause {
                    NativeMarkdownPagedBodyError::Compose(
                        omni_markdown_core::MarkdownLineComposeError::Storage(error),
                    ) => error.raw_os_error(),
                    _ => None,
                };
                // 固定字段仅含Discriminant和可选整数；普通构建完全不编译这条诊断。
                // 原sticky赋值及下方真实清理继续原样执行，诊断不消费或替换任何错误。
                eprintln!(
                    "native_body_failure body={:?} view={view:?} compose={compose:?} font={font:?} registration={registration:?} paragraph={paragraph:?} viewport={viewport:?} fallback={fallback:?} semantics={semantics:?} storage_errno={storage_errno:?}",
                    std::mem::discriminant(&cause),
                );
            }
            *self.failure.borrow_mut() = Some(cause);
        }
        self.failed.set(true);
        let pending_mode = self.pending_mode_action.borrow_mut().take();
        drop(pending_mode); // 失败不继续模式点击，也不让旧Native weak在槽借用内析构。
        let find_binding = self.find_highlights.borrow_mut().take();
        drop(find_binding); // NativeWeak归还可能重入；不能跨Body RefMut释放平台控制块。
        self.font_choices.borrow_mut().clear(); // 失败后不让原字素/context字节留在闲置Body中。
        drop(self.mode_viewport.borrow_mut().take());
        drop(self.selection_reveal.borrow_mut().take());
        drop(self.pending_vertical.borrow_mut().take());
        drop(self.pending_page_scroll.borrow_mut().take());
        let _ = self.cancel_word_delete(); // 首错已保留；仍显式归还局部重入Rc可能保活的新增输入槽。
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
        // 失败退出仍清完整线程及排队结果；主因先保存，真实清理次因必须进入同一错误链。
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

    fn advance(&self) -> Result<()> {
        if self.mode_viewport_waits_for_mount() {
            return Ok(()); // 同次Native挂载完成后由原bind方法恢复唯一调度，不轮询也不发布文首中间帧。
        }
        if self.word_input_is_copying() {
            return Ok(()); // 原Native载荷转换尚在栈内，只由该转换的真实完成恢复原调度。
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
                // 原ruler已完成tile，先使唯一有限正文跟随实际clip；结束所有槽借用后才能进入可重入Native几何。
                viewport.synchronize()?;
                self.schedule()?;
                return Ok(()); // 位数跨界真实改变clip宽度，原后继回合才捕获request，不混用前后几何。
            }
        }
        let view = self.view()?;
        let request = view.capture_scene_request().inspect_err(|cause| {
            // 临时失败定位只读原 typed cause，不为日志重捕几何或重新进入 Native。
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
            return Ok(()); // 旧cache的真实click先沿唯一语义续体准备；保留原sealed Scene，不另建排版或字体图。
        }
        self.refresh_source_target(&request);
        self.refresh_pending_find(&request)?;
        self.refresh_vertical_navigation(&request);
        self.refresh_page_scroll(&request);
        self.refresh_word_delete(&request)?;
        if self.advance_word_typing()? {
            return Ok(()); // 新增文字已按前步收据提交；本回合不再推进下一Core事务或重绘中间态。
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
            // Tab/换格只接纳局部动作，提交前父 Input 与几何仍完全同票。
            // 导航/重挂/关闭必须推进唯一 Producer 的原来源请求，不能复用旧图把动作饿死。
            // Pointer 只等待已有子图的真实 Ready；它不借父来源，仍复用父图避免重复重排。
            && self.pending_cell.borrow().as_ref().is_none_or(|pending| {
                matches!(&**pending, cell::PendingCell::Pointer { .. })
            })
        {
            scene.validate_ready(&|| self.current_body())?;
            return Ok(()); // 重复 clip 通知只复用原同票窗口，不能不断换 nonce 取消正在绘图的候选。
        }
        // 失效业务 Scene 的 Reader 必须在推进新前缀前归还。CPU 已显示的完整帧只持
        // 原弱 key 和像素，保留旧画面无需继续保活旧 Source/Reader 或占第三份快照。
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
        drop(selection_scene); // 只比较原同票Scene；临时claim在后续Source/Reader交接前退出。
        if !keep_word_selection_scene {
            self.clear_scene()?;
        } // 选词未改正文时保留原真实命中图供拖动；任何输入/几何变化与删除仍归还旧Reader。
        let old = self.producer.borrow_mut().take();
        let mut producer = match old {
            Some(producer) if producer.accepts(&request) => producer,
            old => {
                if let Some(old) = old {
                    old.retire()?; // 真归还旧试次及已交付结果；同worker迟到结果仍按原serial独立消费。
                }
                if self.input.is_cell() {
                    self.release_cell_readers()?;
                    if self.input.state().map_err(ViewError::Input)? != request.input {
                        return self.schedule(); // 真实归还中的重入已产生后继输入，不拿旧request补签新Source。
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
                self.input.clear_navigation_geometry(); // 新Source/宽度的换行侧和目标列都须重取真实几何，逻辑选择及历史保持原样。
                // 同一Input原票选择既有Body/Cell工厂；旧Producer已Drop，不占第三份Cell读取。
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
                }; // Cell/Preview 不继承 Body Source 折叠，也不申请其原段落 proof。
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
        // 模式初始化在首Ready之前借唯一Producer的原Source，不能先发布文首再追逐旧caret。
        if let Err(cause) = self.prepare_mode_viewport(&mut producer, &request) {
            #[cfg(feature = "native-close-stage-diagnostic")]
            eprintln!(
                "native_body_phase_failure phase=prepare_mode cause={:?}",
                std::mem::discriminant(&cause)
            );
            return producer::merge_preparation_cleanup(Err(cause), producer.retire());
        }
        // 同源选择可改变marker实际字号；先结束旧字体/Reader并重排，不把已封存的旧图换一张输入票。
        let refreshed = match producer.refresh_semantic_selection(self, &request) {
            Ok(refreshed) => refreshed,
            Err(cause) => {
                #[cfg(feature = "native-close-stage-diagnostic")]
                eprintln!(
                    "native_body_phase_failure phase=refresh_selection cause={:?}",
                    std::mem::discriminant(&cause)
                );
                return producer::merge_preparation_cleanup(Err(cause), producer.retire());
            } // 清理失败与原重排失败共同保留，不能只靠普通Drop丢失Font/Source退休错误。
        };
        if !refreshed {
            *self.producer.borrow_mut() = Some(producer);
            return Ok(()); // 原Font完成包仍归这个唯一Producer；真实通知到达后才恢复准备。
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
        // 已提交Find只在正常唯一Producer已选择原Source后绑定一次；不借旧Ready补签，
        // 不从begin捕Source，也不等无关首屏先发布后再重新推进目标。
        let binding = self
            .bind_pending_find(&producer, &request)
            .and_then(|()| self.bind_selection_reveal(&producer, &request));
        if let Err(cause) = binding {
            return producer::merge_preparation_cleanup(Err(cause), producer.retire());
        } // 绑定失败时Producer尚在局部栈，仍须消费已交付字体结果，不能由?裸丢。

        // producer 在槽借用外运行；Font/Native/current 都可重入，但只能看到真实准备中。
        // 词组删除只推进原Composer的一个有限量子；布局、字体和Reader不另起第二条管线。
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
            // 这里只标识已经返回的原回合；不把可正常退休的 PreparationRevoked 改成另一种错误。
            eprintln!(
                "native_body_phase_failure phase=producer_advance cause={:?}",
                std::mem::discriminant(cause)
            );
        }
        let replaced = match producer.replaced_cell_source(&self.input, &request) {
            Ok(replaced) => replaced,
            Err(cause) => {
                // 原Source复核失败也先实际退休栈内Producer，保主因及同次清理失败，不让?裸丢结果。
                return producer::merge_preparation_cleanup(Err(cause), producer.retire());
            }
        };
        if replaced
            || matches!(
                result,
                Err(NativeMarkdownPagedBodyError::PreparationRevoked)
            )
        {
            // 仅原前台票撤权可交实际后继；Producer 已先核原 Source 首错。整份旧候选
            // 取消并归还，worker 的旧完成仍按旧 serial 消费，不能给新 Source 补签。
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
            return self.finish_word_delete(); // 原Box已放回，Ready才可归还旧读取并消费唯一输入票。
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
            return Ok(()); // 真实glyph恢复改变了clip，旧Scene到此结束；下一原续体完成新帧认证。
        }
        self.finish_source_target()?;
        self.finish_pending_find()?;
        if self.native_front_needs_scene() && self.scene.borrow().is_some() {
            self.schedule()?; // 原Scene真实发布才唤醒未解释event；字体/IO在途时不轮询空转。
        }
        Ok(())
    }

    /// 实际输入、排版和原生几何栈尚未归还时，保存/模式/关闭必须先拒绝而不撤当前owner。
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

    /// 原控制器接受拆图后保留同一完成收据；真实像素事件到达后才允许核验释放完成。
    pub fn begin_close(&self) -> Result<Rc<NativeMarkdownPagedCloseReceipt>> {
        if let Some(receipt) = self.close_receipt.borrow().clone() {
            return Ok(receipt); // 同次重复只取原metadata，不再join或取消后继业务。
        }
        self.preflight_release()?; // 同步资源事务未归还时，原Doc/Native图和Find仍完整保留。
        let view = self.view.borrow().clone();
        let receipt = NativeMarkdownPagedCloseReceipt::new(self.mtm);
        *self.close_receipt.borrow_mut() = Some(Rc::clone(&receipt));
        if let Some(pixels) = view.as_ref().and_then(|view| view.pixel_observer()) {
            receipt.track_pixels(&pixels.0);
        } // 接受后即登记原像素；后续子视图拒绝也不能让失败收据提前声称物理归还。
        if let Err(error) = self.cancel_pending_find() {
            receipt.record(NativeMarkdownPagedCloseFailure::Body(error));
        }
        if let Err(error) = self.cancel_word_delete() {
            receipt.record(NativeMarkdownPagedCloseFailure::Body(error));
        }
        self.active.set(false);
        let pending_mode = self.pending_mode_action.borrow_mut().take();
        drop(pending_mode); // accepted close撤原业务动作，队列只有Weak，不能延长正文或关闭资源。
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
        } // 仅跟随仍在释放的上一张有限像素票，不保退休格列表、Source或业务文档。
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
        // 行号先撤原Scene/字体/像素/client/ruler，再拆正文viewport；同收据核真实Native与munmap。
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
        // 先结束槽借用并真正退休唯一续体，再join；前一步失败也不能跳过线程及排队结果归还。
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
        } // 子视图只交像素原子事实和首错；父收据不等待调用方合法保留的View或Cell parent Doc。
        drop(self.view.borrow_mut().take());
        drop(view);
        let input_result = self.input.close().map_err(ViewError::Input);
        if let Err(error) = input_result {
            receipt.record(NativeMarkdownPagedCloseFailure::View(error));
        }
        self.font_choices.borrow_mut().clear(); // 先归还固定复用槽及其有限输入，不保旧Source或字体弱控制块。
        drop(self.typography.borrow_mut().take()); // 语义原NSFont策略随活动Body归还，不成为idle全局缓存。
        let base_fonts = self.base_fonts.borrow_mut().take();
        drop(base_fonts); // 两种用途的 Native claim 同步归还，不让已关闭 Body 或框架空壳继续持有。
        let fonts = std::mem::take(&mut *self.fonts.borrow_mut());
        drop(fonts); // 所有 RefCell 借用先归还，再执行真实 CF/Native 字体释放。
        drop(self.document.borrow_mut().take());
        let cell = self.cell_session.borrow_mut().take();
        let cell_result = cell
            .as_ref()
            .map_or(Ok(()), |session| session.close().map_err(ViewError::Input));
        drop(cell); // 原显示/worker已归还之后才取消同token并drain；不以Input.close冒充草稿结束。
        if let Err(error) = cell_result {
            receipt.record(NativeMarkdownPagedCloseFailure::View(error));
        }
        // 模式切换归还的是另一份真实 Producer/worker/样式字体；最终关闭不能只证明当前那一份。
        // 这里仍读取原退休收据，不持旧资源，也不以当前字体、Source 已关闭替旧模式补签。
        if self.verify_mode_retirement().is_err() {
            receipt.record(NativeMarkdownPagedCloseFailure::Body(
                NativeMarkdownPagedBodyError::ResourceReleaseFailed,
            ));
        }
        receipt.seal(); // 同步业务清理已返回；只有真实全部像素归还才能交付原完成事件。
        Ok(receipt)
    }

    /// 所有正文Scene失权出口同时退休未发布CPU候选与行号弱快照。
    /// 格关闭等动作可以保留相同Input/几何；不能等新Scene发布才撤旧候选，否则旧续绘
    /// 会在两次有限准备之间把正常退休误报为永久输入失败。已发布像素仍归原CPU owner。
    fn clear_scene(&self) -> std::result::Result<(), ViewError> {
        let pending_link = self.pending_link.borrow_mut().take();
        let scene = self.scene.borrow_mut().take();
        // 这里只克隆本Body实际持有的Rust Rc，不调用Native retain、查询或重新领取Scene。
        // 两个槽及view槽的借用均已结束，候选会话/context的真实析构才可进入可重入Native。
        let view = self.view.borrow().clone();
        if let Some(view) = view {
            view.retire_scene_candidate();
        }
        // 查询任务也先退槽；即使scratch关闭失败，后面的gutter/Scene归还仍全部执行。
        // 调用方按原失败收据或既有cleanup链保首因，不以取消吞掉真实资源错误。
        let link_result = pending_link.map_or(Ok(()), link::PendingLink::retire);
        let gutter = self.gutter.borrow().clone();
        if let Some(gutter) = gutter {
            gutter.clear_snapshot();
        }
        drop(scene);
        link_result
    }

    /// 行号回调的真实失败属于原Body首错，不能另起重试或签一个更新的Scene。
    pub(in crate::native_surface) fn gutter_failed(&self, error: ViewError) {
        self.fail(error.into());
    }
}

impl NativeMarkdownPagedTextViewDelegate for NativeMarkdownPagedBody {
    fn context_menu_client(
        &self,
    ) -> std::result::Result<Option<Rc<Retained<NativeMarkdownPagedTextView>>>, ViewError> {
        if !self.current_body() {
            return Err(ViewError::Revoked);
        }
        self.active_cell_body()
            .map(|child| child.view().map_err(cell::cell_view_error))
            .transpose() // 原活动格保自己的选择与草稿历史；不提交草稿，也不借父 Doc 的 Undo 栈。
    }

    fn current(&self, epoch: u64) -> bool {
        epoch == self.configuration.epoch && self.current_body()
    }
    fn user_scroll(&self, epoch: u64) -> std::result::Result<(), ViewError> {
        if !self.current(epoch) {
            return Err(ViewError::Revoked);
        }
        self.cancel_mode_viewport(); // 只撤旧位置，不改选择或注册新滚动命令；父NSView继续原滚轮交付。
        Ok(())
    }
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
            self.fail(cause); // 原Source首错不能被后续视口动作掩盖成普通边界空操作。
            ViewError::Revoked
        })?;
        let view = self.view().map_err(cell::cell_view_error)?;
        // 滚动本身会换几何，不能用旧 geometry_epoch 拒绝自己的合法后置状态。
        // 稳定权限仍是同 Doc/Cell、完整 InputState 与原 Down；每个 Native step 都重新核这三者。
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
            return Err(ViewError::Pending); // Shift 等纯选词也先交回原结果，不用滚动换几何撤掉自己的首查询。
        }
        self.cancel_mode_viewport(); // 真实手势撤旧模式锚点，不让它把用户重新拉回原位置。
        let viewport = self.viewport.borrow().clone().ok_or(ViewError::Pending)?;
        let result = viewport.scroll_for_drag(point, &current);
        self.find_source_health().map_err(|cause| {
            self.fail(cause); // 即使 Native 重入同时撤权，也先保这条真实来源已经发生的错误。
            ViewError::Revoked
        })?;
        if !current() {
            return Err(ViewError::Revoked);
        }
        result.map_err(|_| ViewError::Revoked)
    }
    fn virtual_viewport(
        &self,
        epoch: u64,
        _bounds_width: f64,
    ) -> std::result::Result<NativeMarkdownPagedVirtualViewport, ViewError> {
        if !self.current(epoch) {
            return Err(ViewError::Revoked);
        }
        let viewport = self.viewport.borrow().clone().ok_or(ViewError::Pending)?;
        // 这是领取“此刻”几何的入口，没有调用方交来的旧票。真实同步重入期间只等新
        // geometry，不能把旧像素或撤掉的矩形当作Ready，也不把正常等候写成永久输入首错。
        // 其他错误仍按原撤权处理；已签Scene/Geometry的消费继续走原严格current检查。
        let geometry = viewport.geometry().map_err(|cause| match cause {
            super::paged_viewport::NativeMarkdownPagedViewportError::Busy => ViewError::Pending,
            _ => ViewError::Revoked,
        })?;
        // 正文坐标从扣除真实行号/页眉遮挡后的frame起点算；raw clip曝露区域仍只归滚动所有者。
        Ok(NativeMarkdownPagedVirtualViewport {
            document_origin: geometry.body_frame.origin,
            layout_origin_x: geometry.layout_origin_x,
            layout_width: geometry.layout_width,
            scroll_epoch: geometry.scroll_epoch,
        })
    }
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
        Err(ViewError::Pending) // 原同一 producer 已排队；这个 Native 事件不保存、不重放。
    }
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
                        // 与创建Scene时同一实际正文起点核对，非零上边距不能偷用raw clip原点。
                        && geometry.body_frame.origin
                            == key.request.virtual_viewport.document_origin
                })
            })
    }
    fn committed(
        &self,
        commit: &NativeMarkdownPagedInputCommit,
    ) -> std::result::Result<(), ViewError> {
        commit.check_current(&|epoch| self.current(epoch))?;
        self.cancel_pending_mode_action(); // 新的真实文字或选择提交撤旧模式点击，不等后继Ready替它补权。
        if !self.owns_word_commit(commit)
            && let Err(cause) = self.cancel_word_delete()
        {
            self.fail(cause); // 后继真实编辑不能把原Word Source首错洗成普通取消。
            return Err(ViewError::Revoked);
        }
        // Canonical 已提交；这里只撤销旧显示并接收同一续体，不假称这次事务尚未发生。
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
            observer.committed(commit); // 原收据只借本同步栈；客户重入后必须继续核同张票，不能补签较新输入。
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
    fn reveal_committed_selection(
        &self,
        commit: &NativeMarkdownPagedInputCommit,
    ) -> std::result::Result<(), ViewError> {
        self.accept_selection_reveal(commit)
    }
    fn activate_inserted_table(
        &self,
        commit: &NativeMarkdownPagedInputCommit,
    ) -> std::result::Result<(), ViewError> {
        self.accept_inserted_table(commit)
            .map_err(cell::cell_view_error)
    }
    fn navigate_vertical(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        action: super::paged_text_view::NativeMarkdownPagedNavigation,
        extend: bool,
    ) -> std::result::Result<bool, ViewError> {
        self.accept_vertical_navigation(request, action, extend)?;
        Ok(true)
    }
    fn delete_word(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        forward: bool,
        permit: super::paged_input::NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<(), ViewError> {
        match self.accept_word_delete(request, forward, permit) {
            Ok(()) => Ok(()), // 只表示原动作已接纳；尚未Ready不能声称发生删除。
            Err(NativeMarkdownPagedBodyError::View(cause)) => Err(cause),
            Err(cause) => {
                self.fail(cause); // 原Source或准备真实失败保留首因，不降成一次成功删除。
                Err(ViewError::Revoked)
            }
        }
    }
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
    fn move_word(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        motion: super::paged_text_view::NativeMarkdownPagedWordMotion,
        extend: bool,
        permit: super::paged_input::NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<(), ViewError> {
        match self.accept_word_move(request, motion, extend, permit) {
            Ok(()) => Ok(()), // 这里只接纳一次系统动作；原Bidi/组边界Ready后才发布选择。
            Err(NativeMarkdownPagedBodyError::View(cause)) => Err(cause),
            Err(cause) => {
                self.fail(cause); // 仅真实Source/资源/调度失败沿原Body首错；非Uniform在队列中单项拒绝。
                Err(ViewError::Revoked)
            }
        }
    }
    fn select_word(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        selection: super::paged_text_view::NativeMarkdownPagedWordSelection,
        gesture: Option<Weak<()>>,
        permit: super::paged_input::NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<(), ViewError> {
        match self.accept_word_selection(request, selection, gesture, permit) {
            Ok(()) => Ok(()), // 接纳真实词组查询；完成选择只由原Source的Ready及同张permit触发。
            Err(NativeMarkdownPagedBodyError::View(cause)) => Err(cause),
            Err(cause) => {
                self.fail(cause); // 源/索引/资源首错仍归原Body，不能把失败伪装成选词成功。
                Err(ViewError::Revoked)
            }
        }
    }
    fn defer_key_down(
        &self,
        event: &objc2_app_kit::NSEvent,
        permit: &super::paged_input::NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<bool, ViewError> {
        self.cancel_mode_viewport(); // 实际key即使暂由原队列等待caret，也不能让旧模式目标在它之前拉回视口。
        match self.accept_native_key(event, permit) {
            Ok(accepted) => Ok(accepted),
            Err(NativeMarkdownPagedBodyError::View(cause)) => Err(cause),
            Err(cause) => {
                self.fail(cause);
                Err(ViewError::Revoked)
            }
        } // 实际Source/资源首错归原Body，额度拒绝保原已接纳event。
    }
    fn has_native_input_barrier(&self) -> bool {
        self.native_input_pending()
    }
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
        } // 菜单与原key同序；不在未来重读已改变的clipboard载荷。
    }
    fn defer_attachments(
        &self,
        paths: Vec<std::path::PathBuf>,
        drop: Option<(u64, usize)>,
        permit: &super::paged_input::NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<bool, ViewError> {
        // 附件仍由原Word链拥有；文件复制只在队首发生，关闭/撤权会同步释放未处理路径。
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
    fn defer_attachment_rejection(&self, message: &str) -> bool {
        self.retain_attachment_rejection(message)
    }
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
        } // 只认证这一解释回调的真实before/Commit，不能以全局depth认领外部提交。
    }
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
        } // 原Commit已经发布，后果错误保留实际首因，由View包装CommittedEffect。
    }
    fn defer_document_motion(
        &self,
        end: bool,
        extend: bool,
        permit: &super::paged_input::NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<bool, ViewError> {
        match self.accept_document_motion(end, extend, permit) {
            Ok(accepted) => Ok(accepted), // 只接纳原端点语义；未来长度/anchor仅来自前步真实收据。
            Err(NativeMarkdownPagedBodyError::View(cause)) => Err(cause),
            Err(cause) => {
                self.fail(cause); // 实际Source或调度失败保持首因，不伪造端点已到达。
                Err(ViewError::Revoked)
            }
        }
    }
    fn defer_source_command(
        &self,
        command: omni_markdown_core::MarkdownEditingCommand,
        permit: &super::paged_input::NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<bool, ViewError> {
        match self.accept_source_command(command, permit) {
            Ok(accepted) => Ok(accepted), // 接纳仅固定语义；列表续行、缩进和历史仍由后续原Core结果决定。
            Err(NativeMarkdownPagedBodyError::View(cause)) => Err(cause),
            Err(cause) => {
                self.fail(cause); // 真正源/资源/调度失败保持首因，不假报命令已执行。
                Err(ViewError::Revoked)
            }
        }
    }
    fn defer_grapheme_delete(
        &self,
        forward: bool,
        permit: &super::paged_input::NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<bool, ViewError> {
        match self.accept_grapheme_delete(forward, permit) {
            Ok(accepted) => Ok(accepted), // 仅说明动作保序接纳；真实范围及正文事务仍归原唯一Producer。
            Err(NativeMarkdownPagedBodyError::View(cause)) => Err(cause),
            Err(cause) => {
                self.fail(cause); // 原源、资源或调度错误保留首因，不能伪报删除成功。
                Err(ViewError::Revoked)
            }
        }
    }
    fn defer_insert(
        &self,
        text: &objc2::runtime::AnyObject,
        permit: &super::paged_input::NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<bool, ViewError> {
        match self.accept_word_typing(text, permit) {
            Ok(accepted) => Ok(accepted), // false沿原同步输入；true只表示有限载荷已在原链按序接纳。
            Err(NativeMarkdownPagedBodyError::View(cause)) => Err(cause),
            Err(cause) => {
                self.fail(cause); // 真实Source或调度失败保首因，不能假报字符已提交。
                Err(ViewError::Revoked)
            }
        }
    }
    fn copy_selection(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
    ) -> std::result::Result<(), ViewError> {
        self.copy_source_selection(request)
    }
    fn scroll_page(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        down: bool,
    ) -> std::result::Result<(), ViewError> {
        match self.accept_page_scroll(request, down) {
            Ok(()) => Ok(()),
            Err(NativeMarkdownPagedBodyError::View(cause)) => Err(cause),
            Err(cause) => {
                self.fail(cause); // 原Source/资源首错保留在Body；不能把未知内部失败伪装成已完成滚动。
                Err(ViewError::Revoked)
            }
        }
    }
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
            _ => Err(ViewError::Pending), // 正式RequiredRow导航仍由原表格控制器接线，不把Tab/Return插入Cell正文。
        }
    }

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
                // 原 Core 已 seal 而 Native 栈尚未归还时，后续真实 Drag/Up 属于父手势，
                // 不能再从已结束草稿捕获 IME origin；物理 child 仍由原 queued detach 负责归还。
                if self
                    .pending_cell
                    .borrow()
                    .as_ref()
                    .is_some_and(|pending| matches!(&**pending, cell::PendingCell::Closing { .. }))
                {
                    return Err(ViewError::Pending); // Esc 的同请求归还尚在途，先保原 canonical authority。
                }
                return Ok(false);
            }
            Err(cause) => return Err(cause.into()), // 其它源/输入失败不能被“草稿已结束”掩盖。
            Ok(_) => {}
        }
        child
            .view()
            .map_err(cell::cell_view_error)?
            .handle_marked_mouse(event)
    }

    fn cell_pointer(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        pointer: super::paged_text_view::NativeMarkdownPagedCellPointer,
        permit: super::paged_input::NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<bool, ViewError> {
        self.accept_cell_pointer(request, pointer, permit)
    }

    fn finish_cell_pointer(
        &self,
        _request: &NativeMarkdownPagedSceneRequest,
    ) -> std::result::Result<(), ViewError> {
        Ok(()) // Up权威由原View槽立即撤销；在途Ready只完成终点选择，不重新打开指针寿命。
    }

    fn extend_cell_pointer(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        extension: super::paged_text_view::NativeMarkdownPagedCellExtension,
        permit: super::paged_input::NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<bool, ViewError> {
        self.extend_cell_selection(request, extension, permit)
    }

    fn defer_pointer(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        pointer: super::paged_text_view::NativeMarkdownPagedPointerLocation,
        permit: super::paged_input::NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<bool, ViewError> {
        self.defer_cell_pointer(request, pointer, permit)
    }

    fn cancel_deferred_pointer(&self) -> bool {
        self.cancel_deferred_cell_pointer()
    }

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
        // 正常业务必须显式begin_close并核验原完成收据；早退只做有限、无重建的兜底断图。
        let _ = self.begin_close(); // Drop只接受清理，不把未订阅/未核验收据当释放成功。
    }
}
