//! 原 Composer 的单一有限续体；前缀目录保存在原匿名文件，不缓存整篇行或字形。

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
// 行尾Source/GCB查询与原part度量共享一次真实量子；跨行继续时不重新领取字节或记录额度。
const LINE_METRIC_BUDGET: MarkdownLineComposeBudget = MarkdownLineComposeBudget {
    bytes: 4096,
    scalars: 256,
    records: 256,
    metadata_units: METADATA_PER_TURN,
};
const MAX_WINDOW_LINES: usize = 256;
// 远端目标只延后至多32条真实LineEnd的checkpoint；不改变原Core每步读取/标量/元数据配额。
const MAX_TARGET_BATCH_LINES: usize = 32;

#[path = "actual_y.rs"]
mod actual_y;
use actual_y::{ActualTarget, MeasuredLineAnchor, TargetAdvance, VerticalTarget};
mod cell_word;
mod grid;
use grid::{Grid, TableWidths};

/// 同一原语义账本的具体字段相加；已处理字节与实际workspace分别保持，不把容量当源读取。
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

/// 同次准备失败与真实退休失败都保留；只有实际Source存储errno提升到错误链首位。
/// 此处只处理本Body的具体typed原因，不从字符串或普通取消推断磁盘故障。
pub(super) fn merge_preparation_cleanup(primary: Result<()>, cleanup: Result<()>) -> Result<()> {
    match (primary, cleanup) {
        // 一侧成功就完整保留另一侧结果；没有实际双错时不构造复合失败。
        (Ok(()), result) | (result, Ok(())) => result,
        (Err(cause), Err(cleanup)) => {
            let (cause, cleanup) = if source_io(&cleanup) && !source_io(&cause) {
                (cleanup, cause) // 清理末尾才发生的Source首errno优先，原操作错误仍作为同次次因保留。
            } else {
                (cause, cleanup) // 两个普通失败或已有Source首因时保留原操作顺序。
            };
            Err(Error::PreparationCleanup {
                cause: Box::new(cause),
                cleanup: Box::new(cleanup),
            })
        }
    }
}

/// 只识别本退休路径已存在的typed存储errno链；纯权限失效不能改变首错优先级。
fn source_io(error: &Error) -> bool {
    use omni_markdown_core::{
        MarkdownDocumentBufferError as Document, MarkdownLineComposeError as Compose,
    };
    // 样式退休会把原Source首错包在复合错误中；包装不能降低它在后续清理合并中的优先级。
    fn compose_source_io(error: &Compose) -> bool {
        match error {
            Compose::Source(
                Document::OriginalBackingUnavailable(_)
                | Document::EditStorageUnavailable(_)
                | Document::DisplayStorageUnavailable(Some(_)),
            ) => true,
            Compose::StyleResetCleanup { cause, .. } => compose_source_io(cause),
            _ => false, // 字形文件自身归还失败仍是清理错误，不能冒充正文读取失败。
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
        _ => false, // 弱scope、旧Cell标记及普通撤权不提升为真实Source I/O首错。
    }
}

/// 准备退休与实际绘图共用这一个View错误分支；只有真实存储errno能压过普通撤销。
/// 首因已经是Source时保留其顺序，不能把目录撤权或同步状态损坏伪装成磁盘失败。
pub(super) fn view_source_io(error: &ViewError) -> bool {
    use crate::native_surface::paged_input::NativeMarkdownPagedInputError as Input;
    use omni_markdown_core::MarkdownDocumentBufferError as Document;
    // 标题查询也读同一原Source；其匿名slug/mask存储不冒充正文I/O。
    // 仅沿这个新错误的实际Cleanup包装识别原Source首因，不能因查询取消改变先后顺序。
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

/// 同一真实字体响应只构造一次；缓存命中与 worker 完整试次都重核当前原 Selection。
/// 字体选择事实不能扩大来源、覆盖或相邻语义权，最终消费仍交回签发请求的 Core 组合器。
fn selected_font_response(
    owner: &Owner,
    layout_source: &MarkdownLayoutTextSource,
    request: MarkdownLineStyleRequest,
    selection: super::semantics::Selection,
    base: &crate::native_surface::paged_line_fonts::NativeMarkdownLineFont,
    font: &crate::native_surface::paged_line_fonts::NativeMarkdownLineFont,
) -> Result<omni_markdown_core::MarkdownLineStyleResponse> {
    // 这个 style 只是原样式字体选择；首 GCB 试次不证明整个 request 无
    // 缺字。后继原 Composer 对实际范围整形，真实 painter 缺字即拒绝候选。
    selection.require(owner, layout_source, &request)?;
    let boundary = selection.style_boundary(owner, layout_source, &request)?;
    let inset =
        crate::native_surface::preview_paragraph::PreviewParagraph::from_styles(selection.styles())
            .map_or(0, |geometry| geometry.inset_26_6());
    let response = request
        .respond_registered(selection.coverage, font.registered(), selection.style)?
        // 原语义基础字体随同一响应封存；Core 在已证明的有限字形段内
        // 审核后继字素的真实字体选择，不能由一次回退成功吞掉后续空格。
        .with_preferred_registered_font(base.registered())?
        .with_paragraph_inset(inset)?;
    let response = if let Some(boundary) = boundary {
        // 只装入同Doc、同来源及原nonce签出的真实相邻样式；它本身不授予闭项，
        // Core仍须实际试完整右字素并确认Font变化，颜色变化不能制造整形边界。
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
    // 多段外 GCB 的首个真实 shaping 段可能仍在原 UBA 元数据续体中；未 Ready 前不伪造 options。
    pending_probe: Option<MarkdownLineFontProbe>,
    selection: super::semantics::Selection,
    in_flight: bool,
    result: Option<NativeMarkdownFontTrialResult>,
    // 每个完整字素都先试原语义的优先字体；上一字素的回退结果不取得后继空格或文字的优先权。
    base: Rc<crate::native_surface::paged_line_fonts::NativeMarkdownLineFont>,
    // 只保当前完整字素的有限纯值键；只有真实 Selected 且原字形资源已归还才存入 Body 缓存。
    cache_key: Option<super::font_choice::FontChoiceKey>,
}

/// 原线程结果只回同一真实 Composer；Cell 的局部范围不能误投外层 canonical 正文。
#[derive(Clone, Copy)]
enum FontTrialTarget {
    Body,
    Cell { row: usize, column: usize },
}

/// 只说明本次真实样式交付；缓存响应已被Core消费，等待则仍由原语义/字体续体拥有。
enum StyleStart {
    Cached { source_bytes: usize },
    Pending,
}

/// 活动 Body 只有一个实际 worker；无 packet 时阻塞接收，不轮询、不定时唤醒。
pub(super) struct FontWorker {
    sender: Option<mpsc::SyncSender<(u64, NativeMarkdownFontTrial)>>,
    receiver: mpsc::Receiver<(u64, NativeMarkdownFontTrialResult)>,
    join: Option<JoinHandle<()>>,
    // 保留主线程最后一份 claim 到 join 返回；MainThreadBound::Drop 在后台会同步投递
    // 主线程，不能让它和主线程 join 形成互等。通知仍只持同一 Weak 的短期副本。
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
                    // 完成通知只有原弱身份和 serial；结果固定事实留在原 channel，不能由
                    // 回调保活 Source、Native 字体、ShapeJob 或第二份正文。
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
                            return; // 旧worker已真实join/归还；迟到通知不能从后继worker领取同serial结果。
                        }
                        if owner.preparing.get() {
                            // 嵌套 Native runloop 只能记下真实完成事件；正在借出的 producer
                            // 要先回到原槽。单 worker 加一个排队 packet 最多容纳两个通知。
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

    /// 只有实际结果通知到达才消费一次队列；这不是探测线程完成的轮询。
    fn take_delivered(&self) -> Result<(u64, NativeMarkdownFontTrialResult)> {
        self.receiver.try_recv().map_err(|_| Error::WorkerFailed)
    }

    pub(super) fn close(mut self) -> Result<()> {
        drop(self.sender.take());
        let mut result = self
            .join
            .take()
            .map_or(Ok(()), |join| join.join().map_err(|_| Error::WorkerFailed));
        // join后唯一results发送端已析构；这里只排空已经完成的真实结果，不轮询线程或等待新工作。
        // join失败也继续消费已交付结果，不能把其资源首错留给receiver的无声Drop。
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
    /// 同一Source/排版宽epoch只记一个真实最大右端；离屏行退休后不留全表列索引。
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
    /// 只保一个原行目录恢复续体；完成后进入可见行集合，离屏后不保历史 Native 索引。
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
    /// 原词组槽需要的唯一离屏格恢复；不保旧Scene，取消先归还同Request在途来源。
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

/// A weak query bound to the original output; a later prefix replaces this ticket without another reader or Source.
struct VisualWork {
    query: Option<omni_markdown_core::MarkdownComposedVisualQuery>,
    collapse: Option<MarkdownComposedLineLookup>,
}

/// 只记原Composer连续LineReady的有限区间和末行事实，不保存全部行或字形。
struct QueuedLines {
    first: usize,
    count: usize,
    last: MarkdownComposedLineFacts,
}

/// 单一真实prefix Reader中的至多32个弱行handle，顺序度量后立即归还小队列。
struct PendingBatch {
    output: Rc<RefCell<MarkdownComposedLines>>,
    first: usize,
    count: usize,
    cursor: Option<MarkdownComposedLine>,
    handles: VecDeque<MarkdownComposedLine>,
    collecting: bool,
}

/// 只保首屏原末行号和真实高度；从最新唯一 Reader 的原索引定位，不额外保一份快照。
/// 这两个标量仅在同一 Producer/Source/宽度生命周期有效，不能跨编辑或布局继承。
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

/// 已取得 LineEnd 仍不授权一次遍历任意多 part；每次恢复同一度量游标。
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
    handles: VecDeque<NativeMarkdownMeasuredLine>,
    ranges: VecDeque<std::ops::Range<usize>>,
    paragraphs: VecDeque<std::ops::Range<usize>>,
    folded_ends: VecDeque<Option<usize>>,
    simple_ltr: bool,
    metrics: Option<LineMetrics>,
    /// 仅当前窗口的真实表格行顶，来自本次完整 LineMetrics，不估计历史行高。
    table_rows: VecDeque<(usize, f64)>,
}

/// 原 Reader 的真实查询与恢复格同寿；没有另一个正文、Parser、Input 或全表坐标缓存。
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
        } // 只在原 Source composer 首次 step 前交有限物理行 proof，不建立另一正文或字形图。
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
    pub(super) fn accepts(&self, request: &NativeMarkdownPagedSceneRequest) -> bool {
        self.domain.same(&request.target)
            && self.generation == request.input.generation
            && self.fold_revision == request.input.fold_revision
            && self.source.utf16_len() == request.input.utf16_len
            && width(request, self.padding).is_ok_and(|width| width == self.width)
    }

    /// 选择变化只重排同源的样式图，不能复用旧隐藏字形，也不能重建原语言索引。
    /// 返回false只表示原Font worker尚未实际交回；其完成事件负责唤醒，不在主线程轮询。
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
            return Ok(true); // Source模式或真实选择未变化，原prefix/Reader与字体图原样保留。
        }
        let ticket =
            PreparationTicket::new(request, &self.source, self.serial, self.width, self.padding);
        ticket.require_pure(owner, &self.source)?;
        if let Some(mut trial) = self.trial.take() {
            if let Some(fallback) = trial.fallback.take() {
                fallback.close(); // 先取消旧Font试次，旧选择不能把完成包交给新语义。
            }
            if trial.in_flight {
                self.trial = Some(trial);
                return Ok(false); // 保留唯一结果槽，收到原worker包并退休后才允许重排。
            }
            if let Some(result) = trial.result.take() {
                result.retire(false)?;
            } // 真实源/Font清理失败仍沿原失败出口，不能被新的空图覆盖。
        }
        // 只有所有已发布行都已完成原度量时，Native 累计 Y 才与同 Store 的最新
        // LineEnd 一致。半批次仍走旧重排，不丢掉已封存但尚未计入高度的行。
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
        ); // 只在原选择换代边界报告真实拥有槽；区分原拒绝门，不为获得保留而清槽，也不记录正文。
        self.pending_style = None;
        self.pending_control = None;
        self.grid = None;
        self.grids.clear();
        self.restoring_grid = None;
        self.cell_word = None; // 选择已经换代；同次旧Request及其有限格读取图随后一同归还。
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
        drop(self.output.take()); // Body已先清旧Scene；即使字形不变，旧输出身份也不为新Input补票。
        ticket.require_pure(owner, &self.source)?;
        if retain_composer {
            // 原 SourceRequest 已在上方同栈退休旧 publication；字形、原 Font 注册和
            // 资源 scope 保持原 owner。重新 dup 同 writer 的已提交前缀，旧弱 Line/Part
            // 仍然失效；后继 Scene/CPU 使用完整新 Input 和新输出身份，不复用旧帧。
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
        // 同 Source 的选择显隐没有撤销刚提交的上下游意图；仅淘汰旧目标列与 BiDi part。
        // 新 Source/宽度入口仍走完整 clear_navigation_geometry，不能把这条窄保留扩展到换代。
        owner.input.clear_style_navigation_geometry();
        ticket.require_pure(owner, &self.source)?;
        Ok(true)
    }
    /// Cell真实编辑使旧读取失效；只在同Input已发布后继内容版本时交回准备，原EIO仍直接失败。
    pub(super) fn replaced_cell_source(
        &self,
        input: &crate::native_surface::paged_input::NativeMarkdownPagedInput,
        request: &NativeMarkdownPagedSceneRequest,
    ) -> Result<bool> {
        if !input.is_cell() {
            return Ok(false); // 原正文冻结Source的既有读取与预算完全保持。
        }
        match self.source.byte_offset_for_utf16(0) {
            Ok(_) => Ok(false),
            Err(omni_markdown_core::MarkdownDocumentBufferError::PreparedTextInputStale)
                if self.source.cell_draft_authority().is_some()
                    && input
                        .state()
                        .is_ok_and(|actual| actual.generation != request.input.generation) =>
            {
                Ok(true) // 构造时domain已核同原Cell；不是任意Source错误或取消会话的静默恢复。
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
        Ok(self.source.authority()) // 只借已有冻结Source的原身份，不捕Source或领取prefix Reader。
    }
    pub(super) fn target_source_health(&self) -> Result<()> {
        self.source
            .byte_offset_for_utf16(0)
            .map_err(ViewError::Document)?;
        Ok(())
    }
    /// 原词组提交在Drop前取得同一Composer的弱释放票；不要求output或新增Source读取者。
    pub(super) fn word_retirement_resources(
        &self,
    ) -> Result<omni_markdown_core::MarkdownLineComposerWorkerResourceObserver> {
        Ok(self
            .composer
            .as_ref()
            .ok_or(Error::Closed)?
            .worker_resource_observer())
    }
    /// 词组删除复用原Composer Payload及Source；开始只认证空caret，不构造行、Reader或字体试次。
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

    /// 普通删除只借同一Composer的真实相邻GCB；无字体、行几何或额外全文读取者。
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

    /// 一个原主队列回合只推进一次共享4096B量子；巨型GCB跨步，不按scalar近似截断。
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
        // 原量子的真实读取计入现有Composer进度；不增加用于冒充验收的另一套计数器。
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
        ticket.require(owner, &self.source)?; // 实际Native几何撤权后，旧Ready范围也没有提交权。
        Ok(step.answer)
    }

    /// 取消只归还原Payload中的词组续体；既有布局、Source和Reader仍归这个Producer。
    pub(super) fn cancel_word_delete(&mut self) {
        if let Some(composer) = self.composer.as_mut() {
            composer.cancel_word_delete();
        }
    }

    /// 选词只借原Composer/GCB与原Source；开始动作不捕第二份读取，也不要求当前LineReady。
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
            return Ok(()); // 只登记已接纳的队首；原CellInline真正交回前不启动任何后继读取。
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

    /// 视觉移动只能由原 Composer 决定使用完整 Root 或逐可见段证书；原选择类逻辑组不借此扩权。
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

    /// 视觉Word仅在原全源UBA明确UniformZero时可借逻辑组；准备不与GCB共用两份量子。
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
        ticket.require(owner, &self.source)?; // 完成事实不能替代原Native几何/输入票的后置检查。
        Ok(step.scope)
    }

    /// 同一主队列回合只消费一份原4KiB/256量子；记录继续计入原Producer真实工作统计。
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
        ticket.require(owner, &self.source)?; // geometry或输入撤权后，真实范围也没有原permit的提交权。
        Ok(step.answer)
    }

    /// 只移除同槽的group续体；原字体、布局与Source仍由这个唯一Producer持有并真实关闭。
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
                self.cell_word = Some(query); // 取消意图不等于SourceRequest单格任务已真实交回。
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

    /// 真实 EOF 才能终止补前缀；尚在途的表格格也以当前实际度量高度参与这份判断。
    pub(super) fn needs_document_prefix(&self, bottom: f64) -> bool {
        !self.complete && self.height() < bottom
    }

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

    /// 活动格导航只在原显示来源/Font 已交还后短借唯一 Request；
    /// 同一在途导航继续自己的量子，不能被普通视口再次启动的 CellInline 抢走归属。
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

    /// Esc 只归还原导航的实际来源作业；未启动的意图直接结束，不能撤整个 Request 重发预算。
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
        self.target_source_health()?; // 同一原 Source 的首错高于正常取消或来源撤权。
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

    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn root_leaf_publication(
        &self,
    ) -> Option<std::rc::Weak<omni_markdown_core::VerifiedMarkdownViewportSources>> {
        self.semantics
            .as_ref()
            .and_then(super::semantics::RootLeaf::publication)
    }

    #[cfg(feature = "native-markdown-runtime-probe")]
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
        // 原Reader单批最多256字形；固定栈只在本观察栈复用，不构造第二个排版或源读取图。
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
                } // 原Newline/空行不是假字形字体断言。
                let authority = part.font_registration().authority()?;
                let index = fonts
                    .iter()
                    .position(|font| font.native().core_font().permits(&authority))
                    .ok_or(Error::Unsupported)?;
                fonts[index].require(part.font_registration())?;
                // 只读取原LineEnd已经封存的glyph。大part保留原metadata观察能力，
                // 但明确没有完整glyph结果；不能扩容或重形后把另一结果当原输出。
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

    /// 链接查询只消费本Producer的原Source和真实Ready，不捕新文档或在鼠标栈推进Parser。
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
                return Ok(link); // 只有真实格覆盖才能完成；没有覆盖仍交普通正文的原恢复路径。
            }
        }
        let semantics = self.semantics.as_ref().ok_or(ViewError::Pending)?;
        semantics.link_at(owner, &self.source, index)
    }

    /// 链接回查借原Composer/字体owner；在途试次只等原通知，已交付试次先沿原完成路径退休。
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
            return Ok(None); // 不并行推进语义或取消真实FontTrial，也不同步join正在后台使用的Source。
        }
        for grid in self.grids.iter().chain(self.grid.iter()).chain(
            self.restoring_grid
                .iter()
                .filter_map(|restoring| restoring.grid.as_ref()),
        ) {
            // mouseUp 仍持原 Scene/Source/手势票；格目标只消费实际 Display 的原 Parser 发布。
            // 范围相等、可见 LINK 位或目标字符串都不能替代这次真实来源查询。
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

    /// 显式退休先结束同次fallback取消权，再实际Drop本Producer；结果首错不能藏在Drop返回之外。
    pub(super) fn retire(mut self: Box<Self>) -> Result<()> {
        let backing = self.source.original_backing_observer();
        self.cancellation.cancel();
        let result = if let Some(trial) = self.trial.as_mut() {
            if let Some(fallback) = trial.fallback.take() {
                fallback.close(); // 同原取消位先置位，精确旧Cell撤权才可作为正常退休。
            }
            trial
                .result
                .take()
                .map_or(Ok(()), |result| result.retire(false).map_err(Error::from))
        } else {
            Ok(()) // 尚未创建试次时没有结果可消费，不构造假零收据。
        };
        drop(self); // 所有原Source、Reader、语义、Composer和剩余试次真实退场后才核末Drop错误。
        if let Some(errno) = backing.as_ref().and_then(|backing| backing.first_errno()) {
            if matches!(
                &result,
                Err(Error::Fallback(crate::native_surface::paged_font_fallback::NativeMarkdownFontFallbackError::Source(
                    omni_markdown_core::MarkdownDocumentBufferError::OriginalBackingUnavailable(original),
                ))) if *original == errno
            ) {
                return result; // 同一observer回读的同一个errno不是第二笔清理失败，不构造假复合错误。
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
        // 新prefix可已建立Reader但尚在有限度量中；观察原同一实例，不为收据publish/dup新Reader。
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

    /// 只聚合此刻实际持有的有限格观察者；不创建 Reader、保留旧格或补造历史释放事实。
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

    /// 只归还原弱目录游标；Source 和 Reader 仍由唯一 Producer 退休路径释放。
    pub(super) fn cancel_visual(&mut self) {
        // 普通排版每回合也会走到这里；空槽没有要归还的游标，不搬出大型 Option。
        // 真正取消仍先 take 清原槽，再 Drop 原值，保留原资源释放和重入可见顺序。
        if self.visual.is_some() {
            drop(self.visual.take());
        }
        if self.visual_word.is_some() {
            drop(self.visual_word.take());
        }
    }

    /// Alternate visual queries with original font/line work; a directory-read turn cannot also run a Composer quantum.
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

    /// 原真实视觉词组查询复用同一 writer 的新增 prefix；分类进度不因视口还没准备好而从起点重扫。
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
        drop(output); // 原文件/字体查询借用结束后才核 Native 当前票或发布选择。
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

    /// Reuse this same query for immediate Ready navigation and queued continuation, without Native calls or scheduling.
    /// A Pending result leaves every already-read record in place, so entering the queue cannot restart its work.
    pub(super) fn step_visual_intent(
        &mut self,
        request: &NativeMarkdownPagedSceneRequest,
        seed: super::word::VisualIntent,
    ) -> Result<VisualAdvance> {
        self.target_source_authority(request, self.serial)?;
        let Some(output) = self.output.as_ref() else {
            return Ok(VisualAdvance::NeedPrefix);
        };
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
        // Source首错优先于Native前台撤权：错误旧源不能被换新Source或取消动作吞掉。
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
                    Err(cause) // 原前台仍有效时不能把真实Native拒绝归咎于后继事件。
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
            return Ok(()); // 原唯一来源槽交回前，普通布局也不得启动后继CellInline。
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
            // 原目标取消或被另一目标替代时也不丢已完成的LineEnd；先封口并完成真实度量，
            // 再领取新目标或普通视口，避免后继旧前缀Ready把尚未度量的小批永远留在槽内。
            self.checkpoint_batch()?;
            owner.schedule()?;
            return Ok(());
        }
        // 确有试次在途时不重复发起；唯一 worker 结束后会投递同一个弱 owner 的完成事件。
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
            return Ok(()); // 同一回合只推进原语义量子或Font准备，不再偷做Composer下一步。
        }
        if let Some(control) = self.pending_control.take() {
            self.provide_control(owner, control, ticket)?;
            owner.schedule()?;
            return Ok(()); // 控制语义与 Native 字体准备独占本次原量子，不在同轮继续测量。
        }
        // FontTrial 已实际交回后才退休完成行，迟到结果不能失去它所属的原格。
        // 未提交行仍在独立 grid 槽，滚回旧视口也不能丢掉原 Core 请求的最终提交责任。
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
            // 视口已移走就归还这一个恢复作业；尚有 CellInline 时先交回原槽再退休。
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
                // 普通滚回不要求把后方在途行排完；明确的后向导航仍由原作业继续取得真实几何。
                // 此刻没有原 CellInline/FontTrial 在途，恢复旧行可以安全短借同一来源请求。
                return self.publish(owner, request, ticket);
            }
            return self.advance_grid(owner, request, ticket);
        }
        // Resolve queued visual work before ordinary viewport publication; missing lines use this same producer.
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
        }); // 向下只补当前与下一实际viewport，不猜系统距离/overlap；上方行已由当前prefix覆盖。
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
                return Ok(()); // 横向范围换票后结束本回合，不能沿旧票再改高度或分页。
            }
            if viewport.geometry()?.content_height != self.height() {
                owner.native_preparation_geometry(&ticket.request, &viewport, || {
                    viewport.set_content_height(self.height())
                })?;
                owner.schedule()?;
                return Ok(()); // 真实前缀高度先安装，旧geometry到此结束，不能先让系统在短prefix底部clamp。
            }
            let result = owner.consume_page_scroll(pending);
            self.target_source_health()?; // 真实Native/current重入后的Source首错优先，动作消费后不可重放。
            result?;
            return Ok(()); // 每轮仅一个标准Native动作；方向段剩余次数交原主队列续体。
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
        // 这一回合只持原四维额度；Core 明确签出的无源目录工作可以消费余额，不能重领配额。
        // 源读取、context、shape 与外部任务仍交回原调度；Source 的缓存桥保持至多一次。
        let mut cache_bridged = false;
        loop {
            ticket.require_pure(owner, &self.source)?;
            // 原Composer可变借用只跨这一个Core调用；Native/Selection检查不能保着它重入。
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
            // 观察的是本回合各字段真实累计；只报最后一步会掩盖已经用掉的目录/标量额度。
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
                        // 本次实际目录工作可能已经生成原 nonce 请求；只接回这一发布事实，
                        // 不再为了取得同一请求额外 step，也不在此读取语义或启动字体工作。
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
                        // 只有已审的原目录/测量阶段可续，不能把零 bytes 当作没有隐含源读取的证明。
                        // 每步仍核原 ticket/Source，至少一项有限余额严格下降；源字节额度始终未用。
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
                        // Preview的Font决策仍可能推进原Parser/metadata；本桥接不把它算作免费样式。
                        // Core若交付实际工作或本次已桥过，也保原请求给后继回合，不重领完整预算。
                        owner.schedule()?;
                    } else if let StyleStart::Cached { source_bytes } =
                        self.start_style(owner, request, ticket)?
                    {
                        // Key读取已原样进入trial_source_bytes；这里只扣共享额度，不重复累计工作。
                        // 本次 Core 四项工作确为零才到这里；此前目录工作已扣除，其余余额不重领。
                        budget.bytes = budget
                            .bytes
                            .checked_sub(source_bytes)
                            .ok_or(Error::CounterExhausted)?;
                        self.maximum_step_bytes =
                            self.maximum_step_bytes.max(quantum.bytes - budget.bytes);
                        cache_bridged = true;
                        // Key 已读源后只保留旧桥原来的一步；后继 Yielded 不再合并到本回合。
                        continue;
                    } else if !self.has_in_flight() {
                        // Pending没有在途worker时仍由原Body续体唤醒；不能依赖不会到达的线程通知。
                        // 已在内部安排的同serial任务会原样去重，不另开轮询或第二个字体试次。
                        owner.schedule()?;
                    }
                    // Pending仍由原worker/续体负责后继唤醒，不在本回合再推进试次或轮询空转。
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
                        // 正式 Source 和 Cell 保持原同步控制响应、原字体与原调度次数。
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
                // 未实现的控制、词典解析、Tab 和无前沿证明不能变成永久 Pending，
                // 更不能退回 TextKit 读取整段；候选明确失败，默认完整后端没有因此切换。
                MarkdownLineComposeAnswer::NeedsResolution(_reason) => {
                    #[cfg(feature = "native-markdown-runtime-probe")]
                    eprintln!(
                        "native_body_unsupported phase=resolution serial={} reason={_reason:?}",
                        self.serial
                    ); // 仅记录原Core范围/原因，不输出正文；真实缺字不能伪装成一般Pending。
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

    /// 格作业与外层 Composer 串行：同回合最多一次原量子或一次真实 Font 准备。
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
        // 即便本轮真实资源操作失败，也先接回已发生的工作量，不能让错误路径漏记读量。
        let measured_width = working.width();
        let work = working.take_work();
        self.record_grid_work(work)?;
        let step = advanced?;
        if measured_width > 0.0 {
            // 列解算器已经固定并量化的前缀宽可授横向范围；这里不授行高或Complete权限。
            self.maximum_content_width = self
                .maximum_content_width
                .max(measured_width + self.padding * 2.0);
        }
        match step {
            grid::Advance::Progress => owner.schedule()?,
            grid::Advance::Style { column, request } => {
                // 格显示坐标只交给该格原 Composer，字体试次仍独占 Body 的同一个工作槽。
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
                // 原空白/换行控制也核同一显示票；不能把正文 control 的权限借给格内源。
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
                // 旧行早已封存；恢复格结束只交回当前可见集合，不能再次向 Core 提交同一行。
                self.finish_grid_restoration();
                owner.schedule()?;
            }
            grid::Advance::Complete => {
                // 所有格真实结束后才回应块行；submitted 只表示响应已交 Core，不表示 LineEnd 已封存。
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
                    return Ok(()); // 原列前缀先进入真实document范围，不能让Native把右列目标夹回首列。
                }
                if viewport.geometry()?.content_height != self.height() {
                    owner.native_preparation_geometry(request, &viewport, || {
                        viewport.set_content_height(self.height())
                    })?;
                    owner.schedule()?;
                    return Ok(()); // 真实Cell前缀高度先进入文档，旧几何不能在短内容范围内夹掉目标。
                }
                ticket.require(owner, &self.source)?;
                // 原 Find 请求的存活检查包围实际 Native reveal，失效目标不能改变当前文档视口。
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

    /// 唯一活格作业来自当前行或当前恢复行，两者共用原 FontTrial 与串行语义槽。
    fn active_grid(&self) -> &Grid {
        match &self.restoring_grid {
            Some(restoring) => restoring.grid.as_ref().expect("恢复格已持实际来源"),
            None => self.grid.as_ref().expect("当前行作业仍在原槽"),
        }
    }
    /// 控制响应必须回原在途格，不能借可见集合里的另一行替换接收者。
    fn active_grid_mut(&mut self) -> &mut Grid {
        match &mut self.restoring_grid {
            Some(restoring) => restoring.grid.as_mut().expect("恢复格已持实际来源"),
            None => self.grid.as_mut().expect("当前行作业仍在原槽"),
        }
    }

    /// 恢复作业只保当前可见格；移交后原查询 Reader/handle 随续体退场，不增加历史库存。
    fn finish_grid_restoration(&mut self) {
        let restoring = self.restoring_grid.take().expect("只有实际恢复作业能完成");
        self.grids
            .push_back(restoring.grid.expect("实际格前缀已经准备完成"));
    }

    /// 一回合只查原目录、重放一个语义量子或推进一个格作业；不在字体线程外另起来源图。
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
            // 原 Reader 自己签发 handle；数字 ordinal 只是查询，不能替裸范围授表格来源权限。
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
        // 原 sealed handle 再次经真实 Reader 验证，列宽/高度不能来自另一个输出或普通文字行。
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

    /// 原Producer接管本轮格账本，所有Cell/自然宽作业退休之后进度仍保留真实已发生读量。
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
            // 累计读量必须保留已退休格的真实成本；溢出时失败，禁止饱和为看似合格的数值。
            *total = total.checked_add(value).ok_or(Error::CounterExhausted)?;
        }
        self.maximum_step_bytes = self.maximum_step_bytes.max(work.maximum_bytes);
        self.maximum_step_scalars = self.maximum_step_scalars.max(work.maximum_scalars);
        self.maximum_step_metadata = self.maximum_step_metadata.max(work.maximum_metadata);
        Ok(())
    }

    /// 原 NeedStyle 仅可交普通字体或真实结构行；表格不走普通段落的 Font 响应。
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
            // 原左语义出版物只借一次紧邻右字素作字体见证；不推进当前语义窗口，
            // 也不把右字素的样式、坐标或输入权交给左侧输出。
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
                        // 远端选择/reveal可把前几条真实LineEnd暂存于原小批；此时height尚未累计它们。
                        // 格top必须来自已经完成原Native度量的前缀，不能以旧height开下一行，
                        // 否则后续批次虽补齐总高度，这一行文字与点击框仍会永久重叠上一表格行。
                        self.pending_style = Some(request);
                        self.checkpoint_batch()?;
                        owner.schedule()?;
                        return Ok(StyleStart::Pending); // 原NeedStyle/nonce尚未消费；先沿原pending_batch逐量子真实度量。
                    }
                    // Core 核真实当前 cursor、原请求 nonce 与行范围，仍在途的前段不能被此交接吞掉。
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
                    return Ok(StyleStart::Pending); // 块交接没有Font响应或假LineReady，后续只推进真实格作业。
                }
                None => {
                    // 同一原 Parser publication 尚未完成，保原 NeedStyle 续体，不构造默认语义。
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

    /// 两种真实坐标域共用同一试次槽和线程；外层PreparationTicket始终核canonical Body。
    fn start_font_trial(
        &mut self,
        owner: &Owner,
        request: MarkdownLineStyleRequest,
        mut selection: super::semantics::Selection,
        target: FontTrialTarget,
        ticket: &PreparationTicket,
    ) -> Result<StyleStart> {
        let layout_source = self.layout_source(target).clone();
        // 内层布局票核显示坐标，外层 ticket 核 canonical 文档；二者各自持有真实权限。
        selection.require(owner, &layout_source, &request)?;
        let semantic_base = if self.semantics.is_some() {
            selection.base_index(owner, &self.source, ticket)?
        } else {
            0
        };
        let probe = self.layout_composer(target).font_probe(&request)?;
        if probe.source_range().len() > 4096 {
            // 回退字体探针仅允许原有限切片；过大请求不得退回整格字符串测量。
            return Err(Error::Unsupported);
        }
        // 本次只使用一个真实 Base；取出该 Rc 就结束 palette 借用，不为每个样式请求
        // 复制整份 Vec 或延长无关字体的强生命期。原 Base 仍贯穿后续校验和 Native 试次。
        let base = Rc::clone(&owner.fonts.borrow()[semantic_base]);
        // continuation 属于 Core 的当前整形段，不是语义字体偏好的替代品。
        // 后继完整 GCB 按同一语义 Base、系统 for_string、系统级联顺序真实试字；
        // Base 再次可用时返回它，才能保留中英混排中的空格宽度与原始换行位置。
        selection.style.points = base.native().point_size() as f32;
        // Key 捕获区间只读原 Core probe、认证字节包络与已捕获字体标量，没有 Native 调用。
        // 因而沿原纯票核当前 Input/来源即可；不能为这三次纯检查重复读取整组 AppKit 几何。
        // probe 的前后 Source/scope 校验仍在原 capture 内；命中后下方 ticket.require 会重新
        // 完整核 Native 几何，未命中后的 fallback 也继续逐 Native 步守原票，不跨重入复用权限。
        let (read, key) =
            super::font_choice::FontChoiceKey::capture(&probe, &selection.style, &base, &|| {
                ticket.pure_current(owner, &self.source)
            });
        // 有限 key 的读取同样属于真实工作；Source、取消或 scope 后验失败也不能把已读量抹成零。
        self.trial_source_bytes = self
            .trial_source_bytes
            .checked_add(read)
            .ok_or(Error::CounterExhausted)?;
        let key = key?;
        let cached = key
            .as_ref()
            .and_then(|key| owner.font_choices.borrow().get(key));
        if let Some(font) = cached {
            // 只复用同完整字素、上下文和原字体选项已验证过的选择；本次原票和语义必须仍有效。
            // 命中不签造 TrialResult，也不把旧 GCB、publication 或字体目录当作本请求权限。
            selection.require(owner, &layout_source, &request)?;
            ticket.require(owner, &self.source)?;
            probe.validate()?;
            let response =
                selected_font_response(owner, &layout_source, request, selection, &base, &font)?;
            self.layout_composer(target).provide_style(response)?;
            // 命中没有worker事件；调用方或在原剩余额度内续一次Core，或按原Grid/语义边界调度。
            // 此返回只带本次实际读取，不持原probe/Selection/响应，更不能冒充后台试次结果。
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
            (None, Some(probe)) // 原唯一 FontTrial 先等首段，不额外捕获字体或启动第二个 worker。
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

    /// 仅路由到已存在的具体正文或当前格，不制造默认Source或后继装配。
    fn layout_source(&self, target: FontTrialTarget) -> &MarkdownLayoutTextSource {
        match target {
            FontTrialTarget::Body => &self.source,
            FontTrialTarget::Cell { row, column } => self.grid_for_row(row).source(column),
        }
    }
    /// 响应回到发起请求的原组合器；迟到格试次不得误交当前正文组合器。
    fn layout_composer(&mut self, target: FontTrialTarget) -> &mut MarkdownLineComposer {
        match target {
            FontTrialTarget::Body => self.composer.as_mut().expect("Body试次持原Composer"),
            FontTrialTarget::Cell { row, column } => self.grid_for_row_mut(row).composer(column),
        }
    }

    /// 按原物理行路由真实试次；可见旧行恢复也只借当前这一作业，不重新捕获 Source。
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
    /// 可变借用仍按原行起点路由，编程装配错误直接 panic，不以另一个格作为默认接收方。
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

    /// Source 保持原控制字体；预览控制先取得同一真实语义，避免 LF 把隐藏围栏撑成普通行。
    fn provide_control(
        &mut self,
        owner: &Owner,
        request: omni_markdown_core::MarkdownLineInlineControl,
        ticket: &PreparationTicket,
    ) -> Result<()> {
        let selection =
            if request.kind == omni_markdown_core::MarkdownLineInlineControlKind::EmptyParagraph {
                None // 真 EOF 空行没有 Markdown 内容，其字体和字面路径相同。
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
        // 控制项同样只需原选定字体；Rc 独立保活到响应交回，RefCell 借用不跨后续调用。
        // palette 的其余字体继续由原 Body 拥有，无需为这个控制项另建临时 Vec。
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

    fn advance_trial(&mut self, owner: &Owner, ticket: &PreparationTicket) -> Result<bool> {
        let mut trial = self.trial.take().ok_or(Error::Closed)?;
        if trial.selection.context_changed(owner)
            || (trial.fallback.is_none() && trial.pending_probe.is_none())
        {
            // 原selection已经改变：先取消旧试次，不把新publication借给旧结果。
            // 在途packet仍由同一个worker交回后析构，drain完成前不发送第二试次混淆serial。
            if let Some(fallback) = trial.fallback.take() {
                fallback.close();
            }
            drop(trial.pending_probe.take()); // 只有原 scope 的首段等待票，不把它转交新的 selection。
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
            } // Cell旧选择由同一Body真实reset撤整张Grid；不能把局部旧请求投入正文槽。
            retired?; // 原context已换，但真实旧试次清理失败不能被下一样式请求吞掉。
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
        #[cfg(feature = "native-markdown-runtime-probe")]
        let probe =
            owner.take_preparation_probe(&ticket.request, &source, self.composer.as_ref())?;
        let current = || {
            if !ticket.current(owner, &source) {
                return false;
            }
            #[cfg(feature = "native-markdown-runtime-probe")]
            super::preparation_probe::deliver(&probe);
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
                    // 同一个 owner 在真实 Base 缺字后优先系统 for_string，再按原级联顺序继续。
                    // palette 只在实际候选已确定后精确复用，不拿 hidden 或其他字号轮流试正文。
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
                None => {} // 当前真实段无缺字且已释放，仍须原协调器确认后继段或整簇 Complete。
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
            self.record_font_probe_work(step)?; // 后验取消也保留真实元数据、读取与标量量子成本。
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
            return Ok(false); // 同一个候选和原 probe 等待，不提交空 Job 或跳下一字体。
        }
        let next = fallback.next_trial_probed(base.native(), &authority, &fonts, &current);
        // 字节读取先于候选构造/原票后验，失败与不可捕获都已发生真实工作，不能只在 Trial 成功后计账。
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
                // 原字体依赖尚未交付时不能冒充缺字或 Ready；诊断只记录阶段，不保留正文。
                #[cfg(feature = "native-markdown-runtime-probe")]
                eprintln!("native_body_font_trial outcome=pending_dependencies");
                Err(Error::Unsupported)
            }
            NativeMarkdownFontFallbackStep::Exhausted { tried } => {
                // 真实有序候选全部试完仍失败才报告耗尽；不能用默认字体替换未知字形。
                #[cfg(feature = "native-markdown-runtime-probe")]
                eprintln!("native_body_font_trial outcome=exhausted actual_tried={tried}");
                #[cfg(not(feature = "native-markdown-runtime-probe"))]
                let _ = tried;
                Err(Error::Unsupported)
            }
        }
    }

    /// 元数据续体与正文/格量子进入同一实际工作账；不把多段边界查找藏成免费字体选择。
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

    /// 整个外 GCB 的真实候选完成后沿原响应提交；单段与多段共用同一注册、源权限和缓存顺序。
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
        // 真实完整试字决定此 GCB 的字体；回到语义 Base 不要求上一回退字体先缺字。
        // 同来源、同样式和已注册字体仍由原响应与 Core FontRuns 核验及合并。
        let selected = choice.into_captured_font(base.native(), &layout_source, current)?;
        let font = if let Some(selected) = selected {
            if let Some(existing) = fonts.iter().find(|font| {
                selected
                    .core_font()
                    .permits(&font.native().core_font().authority())
            }) {
                // 系统实际候选已在文件快照前严格核为同一字体；这里只复用原 Core 注册身份。
                // 原 GCB 仍由唯一 worker 完整试过，不能因旧字体支持过别的字就跳过试次。
                // 归还本次借出的强边，Body 保留唯一原注册及行度量，不随逐格滚动重复 push。
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
                    selected // Source级联字体同样保原CT浮点；不读取或构造Preview行槽。
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
            // 只诊断原短格开头的完整多单位字素；值来自刚成功交付的原Choice/Font注册。
            // metadata/points均为捕获时已认证的纯值，不调Native getter、不读正文、不留新强边。
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
            // complete_trial_probed 已逐 glyph 检查并真实归还原 Job/分页；
            // 当前 Selection、Source 与响应也已接纳，缓存仅留下字体弱边和有限键。
            owner.font_choices.borrow_mut().remember(key, &font);
        }
        Ok(())
    }

    fn accept_line(&mut self, owner: &Owner, facts: &MarkdownComposedLineFacts) -> Result<()> {
        if facts.kind == omni_markdown_core::MarkdownComposedLineKind::TableRow {
            let grid = self.grid.take().expect("真实TableRow来自当前已完成Grid");
            if !grid.submitted
                || grid.source_range() != facts.source_utf16
                || (grid.height() * 64.0).round() as i64 != facts.y_advance_26_6
            {
                return Err(Error::Unsupported);
            }
            self.grids.push_back(grid); // 下一原回合按真实视口退休；旧行几何已封存在原输出目录中。
        }
        self.scan = None;
        self.top_lookup = None;
        if let Some(target) = self.target.as_mut() {
            target.reset_output();
        }
        if let Some(vertical) = self.vertical.as_mut() {
            vertical.reset_output();
        }
        // 只有尚未到目标首scalar的真实新行才启用有限批次。普通首屏/滚动继续逐行发布，
        // 最后一条目标行即使不足32条也立即封口，不能等匹配尾端或整个段落。
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
            return Ok(()); // 仍每次只执行原Core量子；有限batch只减少真实checkpoint频率。
        }
        // 本槽仅保上一条度量完成的候选。先实际归还，再领取同 writer 的下一前缀，
        // 不让中间候选和旧业务 Scene 一起占用两个 Reader 后再请求第三个 FD。
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
        drop(self.output.take()); // 原业务Scene已由Owner撤掉；先归还旧Reader名额，再同步/dup新prefix。
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
            // 这里只计实际调用的整个checkpoint入口（包含guard/sync/dup），不是隔离fsync耗时。
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
        // 每条LineReady已令原Composer进入下一原阶段，publish不是继续step的许可。
        // 到此仅将这些既有LineEnd签入唯一prefix Reader，绝不制造行或复用旧Output身份。
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
            // previous_line实际包含四条64B读取；至多31次前驱加原末行已在checkpoint读过，
            // 本轮不会同时度量part，保持原Native元数据量子，不一次展开任意长前缀。
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
            return Ok(()); // 弱handle收集与真实Font/part度量分到不同回合，避免叠加扩大工作配额。
        }
        let mut parts = LINE_METRIC_BUDGET;
        // 原32-part配额属于整个度量回合，不能每个短行只消费一个part就废弃余量。
        // 已收集目录至多32行；逐行消费原handle也限制了零part/控制行，既不预读新行也不另建Reader。
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
                break; // 本行尚未完成或共享配额已耗尽；原pending_line和剩余batch在后继量子继续。
            }
        }
        if !batch.handles.is_empty() {
            self.pending_batch = Some(batch);
        }
        Ok(())
    }

    /// 单行续体和远端有限批次共用真实度量提交；调用者提供本回合唯一part余量，不逐行重领预算。
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
            // 原part读取包含Line认证2条、Part2条及真实Directory3条；32-part回合最多224条记录。
        }
        if !ready {
            self.pending_line = Some(line);
            return Ok(false);
        }
        if line.metrics.handle.facts().ordinal != self.lines {
            return Err(Error::Unsupported); // 累计Y只接受原顺序的下一条真实行，拒绝遗漏或重复度量。
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
            return Ok(()); // 有限像素仍消费原viewport；本次只发布空document的真实横向范围。
        }
        if viewport.geometry()?.content_height != content_height {
            owner.native_preparation_geometry(request, &viewport, || {
                viewport.set_content_height(content_height)
            })?;
            // 自有高度事务真实改变了几何。旧请求到此结束，只排同一续体；后继回合
            // 再领取真实新请求，绝不在旧排版回调里捕后继Input/宽度为旧Source签Scene。
            owner.schedule()?;
            return Ok(());
        }
        let needed_bottom = request.virtual_viewport.document_origin.y
            + request.viewport.size.height
            + self.padding;
        let output = self.output.as_ref().cloned().ok_or(Error::Unsupported)?;
        let top_prefix = self.top.as_ref().filter(|top| needed_bottom <= top.height);
        // 已度量目标行的bottom覆盖真实viewport底边即可作为逆扫起点；不必从遥远EOF重走，
        // 也不需要padding以外的行参与本次实际绘制。锚始终属于这个Source/字体/宽度Producer。
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
            self.top_lookup = None; // 几何改变后的旧查找票不为后继视口补签；原 output 仍唯一。
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
                        return Ok(()); // 确有原查找 cursor 在途，下一轮继续原文件索引，不从末行重扫。
                    }
                    MarkdownComposedLineLookupAnswer::Found(line) => {
                        if line.facts().ordinal != last_ordinal {
                            return Err(Error::Unsupported);
                        }
                        self.top_lookup = None;
                        Some(line)
                    }
                    MarkdownComposedLineLookupAnswer::PrefixNotCovered => {
                        return Err(Error::Unsupported); // 已实测首屏应在最新前缀内，缺失是失配而非空 Ready。
                    }
                }
            } else {
                self.top_lookup = None;
                output.borrow_mut().last_line()?
            }
        } else {
            None // 原 scan 已持当前候选的游标，不重复定位或从首屏末行重开。
        };
        // 本轮原窗口移到唯一局部 owner，释放 self.scan 字段借用后才记真实读取工作。
        // 需要继续时完整放回；错误则由外层原失败/关闭流程释放，不漏记失败前已发生的读量。
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
            let line_top = scan.bottom - height;
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
                    // 行顶是同一目录逆扫得到的真实值，只保当前有限窗口，不给所有历史行建 Native 索引。
                    scan.table_rows
                        .push_front((handle.facts().ordinal, line_top));
                }
                scan.handles.push_front(handle);
            }
            scan.bottom = line_top;
            // 一旦已覆盖真实视口顶部，后续更早行没有进入本次有限窗口的资格。
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
            return self.publish_grid(owner, request, ticket); // 正文已在上方，当前表格前缀不制造假文本行。
        }
        if self.begin_missing_grid_restoration(&output, &scan)? {
            self.scan = Some(scan); // 保住本次原窗口，后续只恢复缺失格，不从正文末尾重扫。
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
            self.scan = Some(scan); // 已完成的正文窗口保原弱handles，后续只续格目录，不从正文末行重扫。
            owner.schedule()?;
            return Ok(());
        };
        let palette = owner.fonts.borrow().clone();
        let first_ordinal = scan
            .handles
            .front()
            .ok_or(Error::Unsupported)?
            .facts()
            .ordinal;
        let first_top = NSPoint::new(
            request.virtual_viewport.layout_origin_x + self.padding
                - request.virtual_viewport.document_origin.x,
            request.viewport.size.height + top - scan.first_top,
        );
        // 临时几何归因只打印本次已完成扫描的原值；不重新查询行、Source 或 Native。
        // 同一行在栏高变化前后的 seed 与局部起点可据此区分逆扫误差和后续 baseline 运算。
        #[cfg(feature = "native-close-stage-diagnostic")]
        eprintln!(
            "native_line_geometry_seed first_ordinal={first_ordinal} prefix_height={prefix_height:?} scan_first_top={:?} first_top_y={:?} document_origin_y={top:?} viewport_height={:?}",
            scan.first_top, first_top.y, request.viewport.size.height,
        );
        let window = Rc::new(
            NativeMarkdownComposedViewport::prepare(
                output.clone(),
                key.clone(),
                palette,
                scan.handles,
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
            // 与光标方向共用同一原票；最多1000个整数范围，不读取被选正文或另捕Source。
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
                        // 原票已固定Doc/generation/完整输入；纯Core借用不跨Native，不能用UBA段代替逻辑行。
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
                    None => None, // 未证明方向的选择不从ordered范围猜head，也不消费旧memento。
                }
            } else {
                None // Cell、Preview和多选没有本次Source单有向活动行权限。
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
                        (*number, logical.clone()) // 同一长逻辑行的后续softwrap只复用本次局部标量，不重复查树。
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
            None // Cell和Preview既不挂ruler，也不为行号读取canonical逻辑行。
        };
        ticket.require(owner, &self.source)?; // 所有Doc/Rc借用已结束；发布前仍核原Source和同次Native权限。
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
            return Ok(()); // 查询getter重入已换意图；旧候选不能给后继查询发布空/旧匹配。
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
        drop(highlights); // 原栏/控制器的临时Native claim在安装后归还，重入的新查询可立即撤原Scene。
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

    /// 当前窗口缺少旧格时，只恢复一条真实封存行；已有行不重测宽度，也不建立全表 Native 索引。
    fn begin_missing_grid_restoration(
        &mut self,
        output: &Rc<RefCell<MarkdownComposedLines>>,
        scan: &Scan,
    ) -> Result<bool> {
        for (ordinal, top) in &scan.table_rows {
            let handle = scan
                .handles
                .iter()
                .find(|line| line.facts().ordinal == *ordinal)
                .expect("表格行顶只来自同一窗口真实 handle");
            let height = handle.facts().y_advance_26_6 as f64 / 64.0;
            if *top
                >= scan.request.virtual_viewport.document_origin.y
                    + scan.request.viewport.size.height
                || *top + height <= scan.request.virtual_viewport.document_origin.y
            {
                continue; // 目录边界相接的行没有可见交集，不能反复恢复后又立即退休。
            }
            if self
                .grids
                .iter()
                .chain(self.grid.iter())
                .any(|grid| grid.source_range() == handle.facts().source_utf16)
            {
                continue;
            }
            // 缺失的旧行只创建一个真实 lookup 续体；前一次完成后再接下一可见行，避免全表准备。
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

    /// 所有行共用一个真实扫描预算，完成后才一次装配当前可见格窗口。
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
        // 扫描尚未全部完成前不创建任何 Native 格窗口，防止前格反复分配而后格永远饥饿。
        let mut rows = Vec::new();
        for grid in self.grids.iter_mut().chain(self.grid.iter_mut()) {
            if grid.visible(&key.request) {
                rows.push(grid.prepare(owner, key, current)?);
            }
        }
        Ok(Some(rows.into_boxed_slice()))
    }

    /// 表格首行尚无Core LineReady时也只发布真实Cell前缀；没有伪造的空正文窗口或EOF。
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
            return Ok(()); // 尚未封存整行也可交付已固定列宽，但旧Scene几何不能续签。
        }
        if viewport.geometry()?.content_height != self.height() {
            // 先发布已测真实内容范围；这次改变几何后必须重取 Scene key，不能继续签旧像素坐标。
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
            // 同一 Scene 请求仍待有限扫描续体，保持 Pending 并排下一回合。
            owner.schedule()?;
            return Ok(());
        };
        let input_current = |epoch| owner.current(epoch) && current();
        // 选择来自原 Input 当前权限；Grid-only publication 也不能另建输入图或假设空选择。
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
        // 无普通正文窗口就是实际 None；只有真实格前缀参与绘制，不伪造空 TextWindow。
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

/// 只用canonical行树定位原活动头的逻辑行；完整子树读聚合，命中叶至多4KiB，不扫描整段。
pub(super) fn source_line_range_at_head(
    document: &omni_markdown_core::MarkdownDocumentBuffer,
    head: usize,
) -> std::result::Result<std::ops::Range<usize>, omni_markdown_core::MarkdownDocumentBufferError> {
    source_line_at_head(document, head).map(|(_, range)| range)
}

/// 行号和活动底色共用同一CRLF/尾空行定义；只返回原行树数字与端点，不复制逻辑行文本。
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
        // prefix已计CR，而真实下一行始于完整CRLF后；LF内部仍归前行，不另读正文或改变head。
        (line - 1, document.byte_offset_for_line(line - 1)?)
    } else {
        (line, start) // 普通标量边界沿原行树结果，不把UBA或soft line接缝加入逻辑行。
    };
    let end = if line + 1 < document.line_count() {
        document.byte_offset_for_line(line + 1)?
    } else {
        document.byte_len() // 真EOF没有下一行；若原文件以换行结束，start==end签尾空行。
    };
    Ok((
        line,
        document.utf16_offset_for_byte(start)?..document.utf16_offset_for_byte(end)?,
    ))
}

impl Owner {
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
            return result.retire(false).map_err(Error::from); // 迟到只失去字体选择权，真实资源错误仍须交给原Body。
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
            // 槽借用已经归还；未安装的结果只在此退场，不因状态拒绝而裸丢真实I/O失败。
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
        // 有限未度量批与真实Reader一并归还；取消不能只清目标标志而让批次挂在后继会话。
        self.queued_lines = None;
        drop(self.pending_line.take());
        drop(self.pending_batch.take());
        let trial = self.trial.take();
        if let Some(mut trial) = trial {
            // fallback 的 Drop 请求同一在途 ShapeCancellation；实际 worker 属于 Body，
            // 关闭时断 channel 并 join，不能以取消位代替 ShapeJob 资源归还。
            drop(trial.fallback);
            drop(trial.result.take());
        }
        self.pending_style = None;
        self.pending_control = None;
        drop(self.semantics.take()); // 真正取消唯一来源请求并释放Ready，不靠旧Native任务回收。
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
                    return Ok(false); // 只保原尾游标；不把尚未读到的空白或正文判成已完成宽度。
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
            budget.records -= 7; // Line认证2条、Part2条和真实Directory3条，仍沿原part交易计费。
            match part.facts().kind {
                omni_markdown_core::MarkdownComposedLinePartKind::Glyphs
                | omni_markdown_core::MarkdownComposedLinePartKind::Tab => {
                    self.visible_end = part.facts().source_utf16.end;
                }
                omni_markdown_core::MarkdownComposedLinePartKind::FoldedSection => {
                    // 原可见 glyph/TAB 末端与真实 opaque 控制共同证明 header 行末；
                    // fold 内包含的 CR/LF 和隐藏行不能变成 EndOfLine 或行内删除的目标。
                    self.folded_end = Some(self.visible_end);
                }
                omni_markdown_core::MarkdownComposedLinePartKind::ParagraphBreak => {}
                omni_markdown_core::MarkdownComposedLinePartKind::TableRow => {
                    self.part += 1;
                    continue; // 表格块已持实际完整Cell高度，不借这个目录Font生成普通文本行槽。
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
        // 本窗口的离屏/缺乏几何不是自动重试许可；只有 owner 另行排队真实工作才可 Pending。
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
    /// 复合样式错误中的正文首错不能被随后清理的另一个Source错误调换；两份errno都保留。
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
        // 这里只检验已存在错误类型的传播合同，不冒称注入了内核EIO或munmap故障。
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

    /// 真文件保持到断言结束；活动行测试与生产路径共用canonical mmap行树。
    fn document(content: &str) -> (tempfile::NamedTempFile, MarkdownDocumentBuffer) {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(content.as_bytes()).unwrap();
        let document = MarkdownDocumentBuffer::open(file.path(), 4 * 1024 * 1024).unwrap();
        (file, document)
    }

    #[test]
    /// 原Source逻辑分行不能随双向段边界、emoji宽度或视口soft wrap改变。
    fn logical_line_uses_crlf_not_uba_paragraph_or_soft_wrap_boundaries() {
        // NEL、LS、PS会影响UBA段或字形换行，但不属于原Source的CR/LF逻辑分行。
        let (_file, document) = document("甲😀\r\nab\rcd\nef\u{85}gh\u{2028}ij\u{2029}kl");
        for (head, expected) in [
            (0, 0..5),
            (1, 0..5),
            (3, 0..5),
            (4, 0..5), // CRLF内部只改变观察归属，不改变真正的head或原换行字节。
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
    /// 跨行反向选择、真实尾空行和Undo后的新发布都使用canonical活动head。
    fn active_head_keeps_nonempty_reverse_selection_and_true_eof_line() {
        let (_file, mut document) = document("first\nsecond\n");
        // 反向跨行选择仍以真实head定位；不能取有序选择的末端或只高亮空选区。
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
        // 实际编辑后立即沿新root取行范围；Undo回原root后末尾空行不能被旧范围覆盖。
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
    /// 巨型原单行仍返回完整逻辑范围；空源只授予原EOF空行，不物化正文。
    fn giant_single_line_and_empty_document_need_no_materialized_text() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let block = [b'a'; 4096];
        // 两MiB真实源只有一个逻辑行；测试输入也逐块落盘，不构造整段测试String。
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
