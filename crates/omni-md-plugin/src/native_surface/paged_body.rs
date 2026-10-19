//! Source 正文的显式装配候选；只消费原文档和分页管线，尚不替换默认编辑器。
//!
//! 这个 owner 没有 TextKit、整篇字符串或第二文档。尚未实现的语义明确拒绝；不能
//! 因候选可绘图就声称 Markdown 预览、全部导航或完整产品已经通过验收。

use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::{Rc, Weak},
};

use dispatch2::MainThreadBound;
use objc2::rc::Retained;
use objc2_app_kit::{NSParagraphStyle, NSScrollView};
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
mod navigation;
mod notifications;
mod page_scroll;
mod paint_observer;
mod preparation;
#[cfg(feature = "native-markdown-runtime-probe")]
mod preparation_probe;
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
    /// Source保持既有字面路径；InlinePreview仅显式候选消费已认证RootLeaf字体，不隐藏marker。
    pub presentation_mode: omni_markdown_core::MarkdownPresentationMode,
    pub epoch: u64,
    pub maximum_font_bytes: u64,
    pub padding: f64,
    pub colors: NativeMarkdownPagedBodyColors,
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
    pub target_max_batch_lines: usize,
    /// 只计原目标准备期间实际调用的checkpoint，不包含之前普通首屏/滚动调用。
    pub target_checkpoint_attempts: usize,
    /// 实际checkpoint入口累计耗时，含guard/sync/dup；不能解释为单独fsync的测量。
    pub target_checkpoint_millis: f64,
    /// 批次末行、前驱和part访问实际读取的64B记录数，不与旧前缀Y回溯混计。
    pub target_batch_index_records: usize,
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
    typography: RefCell<Option<semantics::Typography>>,
    catalog: MarkdownLineFontCatalog,
    paragraph: super::paged_paragraph_style::NativeMarkdownLineParagraph,
    // 原续体只分配一次；每轮借出和放回移动唯一Box，避免反复搬运整份排版工作状态。
    producer: RefCell<Option<Box<Producer>>>,
    close_receipt: RefCell<Option<Rc<NativeMarkdownPagedCloseReceipt>>>,
    worker: RefCell<Option<FontWorker>>,
    scene: RefCell<Option<Rc<Scene>>>,
    source_target: RefCell<Option<Rc<target::SourceTargetRequest>>>,
    selection_reveal: RefCell<Option<NativeMarkdownPagedSceneRequest>>,
    pending_vertical: RefCell<Option<Rc<navigation::PendingVertical>>>,
    pending_page_scroll: RefCell<Option<Rc<page_scroll::PendingPageScroll>>>,
    pending_find: RefCell<Option<Rc<find::PendingFind>>>,
    pending_word_delete: RefCell<Option<Rc<word::PendingWordDelete>>>,
    paint_observation: RefCell<Option<PaintObservation>>,
    #[cfg(feature = "native-markdown-runtime-probe")]
    preparation_probe: RefCell<Option<Weak<dyn NativeMarkdownPagedBodyPreparationProbe>>>,
    revoked_preparations: Cell<usize>,
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
    /// 移交原文档 owner 并挂在原 NSScrollView；失败时局部候选沿原释放边归还。
    ///
    /// 调用方在业务关闭后仍须核实 Native Weak、映射和关闭收据；本方法不新增窗口、
    /// 快捷键或标题栏。这个显式候选只覆盖 Source 正文，不授权默认模式切换。
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
            font,
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
            font,
            paragraph,
            configuration,
            mtm,
        )
    }

    fn attach_target(
        target: Attachment,
        scroll: &NSScrollView,
        font: &CTFont,
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
                    return Err(NativeMarkdownPagedBodyError::Unsupported); // 草稿局部坐标不借原Doc的RootLeaf语义。
                }
                let input = NativeMarkdownPagedInput::new_cell(Rc::downgrade(&session))
                    .map_err(ViewError::Input)?;
                (None, Some(session), input)
            }
        };
        let captured =
            NativeMarkdownPagedFont::capture(font, configuration.maximum_font_bytes, mtm, &|| {
                true
            })?;
        let catalog = MarkdownLineFontCatalog::new();
        let paragraph = super::paged_paragraph_style::capture_paged_paragraph_style(
            paragraph,
            None,
            mtm,
            &|| true,
        )?;
        let registered = Rc::new(NativeMarkdownLineFont::register(
            captured,
            &catalog,
            &|| true,
        )?);
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
            fonts: RefCell::new(vec![registered]),
            typography: RefCell::new(
                (configuration.presentation_mode
                    == omni_markdown_core::MarkdownPresentationMode::InlinePreview)
                    .then(|| semantics::Typography::new(font)),
            ),
            catalog,
            paragraph,
            producer: RefCell::new(None),
            close_receipt: RefCell::new(None),
            worker: RefCell::new(None),
            scene: RefCell::new(None),
            source_target: RefCell::new(None),
            selection_reveal: RefCell::new(None),
            pending_vertical: RefCell::new(None),
            pending_page_scroll: RefCell::new(None),
            pending_find: RefCell::new(None),
            pending_word_delete: RefCell::new(None),
            paint_observation: RefCell::new(None),
            #[cfg(feature = "native-markdown-runtime-probe")]
            preparation_probe: RefCell::new(None),
            revoked_preparations: Cell::new(0),
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
            && owner.configuration.presentation_mode
                == omni_markdown_core::MarkdownPresentationMode::Source
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

    pub fn take_failure(&self) -> Option<NativeMarkdownPagedBodyError> {
        self.failure.borrow_mut().take()
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
        });
        Ok(())
    }

    fn fail(&self, cause: NativeMarkdownPagedBodyError) {
        self.fail_pending_find(&cause);
        // 首错是 sticky：停止原续体并清掉 Ready，不用再次捕获较新 Source 隐藏错误。
        if self.failure.borrow().is_none() {
            *self.failure.borrow_mut() = Some(cause);
        }
        self.failed.set(true);
        drop(self.selection_reveal.borrow_mut().take());
        drop(self.pending_vertical.borrow_mut().take());
        drop(self.pending_page_scroll.borrow_mut().take());
        let _ = self.cancel_word_delete(); // 首错已保留；仍显式归还局部重入Rc可能保活的新增输入槽。
        self.revoke_source_target();
        drop(self.paint_observation.borrow_mut().take());
        #[cfg(feature = "native-markdown-runtime-probe")]
        drop(self.preparation_probe.borrow_mut().take());
        self.clear_scene();
        let producer = self.producer.borrow_mut().take();
        let producer_result = producer.map_or(Ok(()), Producer::retire);
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
        if self.word_input_is_copying() {
            return Ok(()); // 原Native载荷转换尚在栈内，只由该转换的真实完成恢复原调度。
        }
        let gutter = self.gutter.borrow().clone();
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
        let request = view.capture_scene_request()?;
        if !self
            .input
            .target_domain()
            .map_err(ViewError::Input)?
            .same(&request.target)
        {
            return Err(ViewError::Revoked.into());
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
            self.clear_scene();
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
                Box::new(Producer::new(
                    source,
                    &request,
                    self.catalog.clone(),
                    self.configuration.padding,
                    self.serial.get(),
                    self.configuration.presentation_mode,
                )?)
            }
        };
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
        let deleting_word = self.pending_word_delete.borrow().is_some();
        let result = if deleting_word {
            self.advance_word_delete(&mut producer, &request)
        } else {
            producer.advance(self, &request)
        };
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
            self.clear_scene();
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
        self.finish_source_target()?;
        self.finish_pending_find()?;
        Ok(())
    }

    /// 原控制器接受拆图后保留同一完成收据；真实像素事件到达后才允许核验释放完成。
    pub fn begin_close(&self) -> Result<Rc<NativeMarkdownPagedCloseReceipt>> {
        if let Some(receipt) = self.close_receipt.borrow().clone() {
            return Ok(receipt); // 同次重复只取原metadata，不再join或取消后继业务。
        }
        let view = self.view.borrow().clone();
        let viewport = self.viewport.borrow().clone();
        let gutter = self.gutter.borrow().clone();
        if self.preparing.get()
            || view.as_ref().is_some_and(|view| view.is_busy())
            || viewport.as_ref().is_some_and(|viewport| viewport.is_busy())
            || gutter.as_ref().is_some_and(|gutter| gutter.is_busy())
        {
            return Err(NativeMarkdownPagedBodyError::Busy);
        }
        drop((viewport, gutter)); // 同步几何事务还没结束时整个关闭不启动，原 Doc 和 Native 图都保留。
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
        self.clear_scene();
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
        drop(self.typography.borrow_mut().take()); // 语义原NSFont策略随活动Body归还，不成为idle全局缓存。
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
        receipt.seal(); // 同步业务清理已返回；只有真实全部像素归还才能交付原完成事件。
        Ok(receipt)
    }

    /// 所有正文Scene失权出口同时撤行号弱快照；Native调用发生在RefCell槽借用结束后。
    fn clear_scene(&self) {
        let scene = self.scene.borrow_mut().take();
        let gutter = self.gutter.borrow().clone();
        if let Some(gutter) = gutter {
            gutter.clear_snapshot();
        }
        drop(scene);
    }

    /// 行号回调的真实失败属于原Body首错，不能另起重试或签一个更新的Scene。
    pub(in crate::native_surface) fn gutter_failed(&self, error: ViewError) {
        self.fail(error.into());
    }
}

impl NativeMarkdownPagedTextViewDelegate for NativeMarkdownPagedBody {
    fn current(&self, epoch: u64) -> bool {
        epoch == self.configuration.epoch && self.current_body()
    }
    fn virtual_viewport(
        &self,
        epoch: u64,
    ) -> std::result::Result<NativeMarkdownPagedVirtualViewport, ViewError> {
        if !self.current(epoch) {
            return Err(ViewError::Revoked);
        }
        let viewport = self.viewport.borrow().clone().ok_or(ViewError::Pending)?;
        let geometry = viewport.geometry().map_err(|_| ViewError::Revoked)?;
        // 正文坐标从扣除真实行号/页眉遮挡后的frame起点算；raw clip曝露区域仍只归滚动所有者。
        Ok(NativeMarkdownPagedVirtualViewport {
            document_origin: geometry.body_frame.origin,
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
        self.clear_scene();
        self.schedule().map_err(|_| ViewError::Revoked)?;
        commit.check_current(&|epoch| self.current(epoch))?;
        Ok(())
    }
    fn reveal_committed_selection(
        &self,
        commit: &NativeMarkdownPagedInputCommit,
    ) -> std::result::Result<(), ViewError> {
        self.accept_selection_reveal(commit)
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
        forward: bool,
        extend: bool,
        permit: super::paged_input::NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<(), ViewError> {
        match self.accept_word_move(request, forward, extend, permit) {
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
        permit: super::paged_input::NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<(), ViewError> {
        match self.accept_word_selection(request, selection, permit) {
            Ok(()) => Ok(()), // 接纳真实词组查询；完成选择只由原Source的Ready及同张permit触发。
            Err(NativeMarkdownPagedBodyError::View(cause)) => Err(cause),
            Err(cause) => {
                self.fail(cause); // 源/索引/资源首错仍归原Body，不能把失败伪装成选词成功。
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
        match action {
            super::paged_text_view::NativeMarkdownPagedCellAction::Cancel => {
                self.cancel_cell(permit)
            }
            _ => Err(ViewError::Pending), // 正式RequiredRow导航仍由原表格控制器接线，不把Tab/Return插入Cell正文。
        }
    }
}

impl Drop for NativeMarkdownPagedBody {
    fn drop(&mut self) {
        // 正常业务必须显式begin_close并核验原完成收据；早退只做有限、无重建的兜底断图。
        let _ = self.begin_close(); // Drop只接受清理，不把未订阅/未核验收据当释放成功。
    }
}
