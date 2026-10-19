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
    MarkdownLayoutFontAuthority, MarkdownLayoutTextSource, MarkdownLineComposeAnswer,
    MarkdownLineComposeBudget, MarkdownLineComposeCancellation, MarkdownLineComposeStyle,
    MarkdownLineComposer, MarkdownLineFontCatalog, MarkdownLineFontProbe, MarkdownLineStyleRequest,
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
        NativeMarkdownFontFallback, NativeMarkdownFontFallbackCompletion,
        NativeMarkdownFontFallbackStep, NativeMarkdownFontTrial, NativeMarkdownFontTrialResult,
        NativeMarkdownProbedFontFallbackRequest,
    },
    paged_line_viewport::NativeMarkdownComposedViewport,
    paged_text_view::NativeMarkdownPagedSceneNonce,
};

const METADATA_PER_TURN: usize = 32;
const MAX_WINDOW_LINES: usize = 256;
// 远端目标只延后至多32条真实LineEnd的checkpoint；不改变原Core每步读取/标量/元数据配额。
const MAX_TARGET_BATCH_LINES: usize = 32;

#[path = "actual_y.rs"]
mod actual_y;
use actual_y::{ActualTarget, MeasuredLineAnchor, TargetAdvance, VerticalTarget};

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
    use crate::native_surface::paged_input::NativeMarkdownPagedInputError as Input;
    use omni_markdown_core::{
        MarkdownDocumentBufferError as Document, MarkdownLineComposeError as Compose,
    };
    match error {
        Error::Fallback(cause) => cause.is_source_io(),
        Error::View(ViewError::Document(
            Document::OriginalBackingUnavailable(_) | Document::EditStorageUnavailable(_),
        ))
        | Error::View(ViewError::Input(Input::Document(
            Document::OriginalBackingUnavailable(_) | Document::EditStorageUnavailable(_),
        )))
        | Error::Compose(Compose::Source(
            Document::OriginalBackingUnavailable(_) | Document::EditStorageUnavailable(_),
        )) => true,
        Error::PreparationCleanup { cause, cleanup } => source_io(cause) || source_io(cleanup),
        _ => false, // 弱scope、旧Cell标记及普通撤权不提升为真实Source I/O首错。
    }
}

struct FontTrial {
    request: MarkdownLineStyleRequest,
    fallback: Option<NativeMarkdownFontFallback>,
    selection: super::semantics::Selection,
    in_flight: bool,
    result: Option<NativeMarkdownFontTrialResult>,
    probe: MarkdownLineFontProbe,
    base_index: usize,
    allow_cascade: bool,
    continuation_font: Option<MarkdownLayoutFontAuthority>,
    continuation_rejected: bool,
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
            result = merge_preparation_cleanup(result, delivered.retire().map_err(Error::from));
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
    width: i64,
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
    semantics: Option<super::semantics::RootLeaf>,
    scan: Option<Scan>,
    pending_line: Option<PendingLine>,
    top: Option<TopPrefix>,
    top_lookup: Option<TopLookup>,
    target: Option<ActualTarget>,
    vertical: Option<VerticalTarget>,
    visual: Option<VisualWork>,
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
    ascent: f64,
    descent: f64,
    leading: f64,
    ltr: bool,
}

struct Scan {
    request: NativeMarkdownPagedSceneRequest,
    cursor: Option<MarkdownComposedLine>,
    bottom: f64,
    first_top: f64,
    handles: VecDeque<MarkdownComposedLine>,
    ranges: VecDeque<std::ops::Range<usize>>,
    paragraphs: VecDeque<std::ops::Range<usize>>,
    simple_ltr: bool,
    metrics: Option<LineMetrics>,
}

impl Producer {
    pub(super) fn new(
        source: MarkdownLayoutTextSource,
        request: &NativeMarkdownPagedSceneRequest,
        catalog: MarkdownLineFontCatalog,
        padding: f64,
        serial: u64,
        mode: omni_markdown_core::MarkdownPresentationMode,
    ) -> Result<Self> {
        request
            .target
            .require_source(&source)
            .map_err(ViewError::Input)?;
        let width = width(request, padding)?;
        let cancellation = MarkdownLineComposeCancellation::new();
        let composer = MarkdownLineComposer::new_with_frontier(
            source.clone(),
            &source.authority(),
            catalog,
            width,
            cancellation.clone(),
        )?;
        let semantics = (mode == omni_markdown_core::MarkdownPresentationMode::InlinePreview)
            .then(|| super::semantics::RootLeaf::new(&source, serial));
        Ok(Self {
            generation: source.generation(),
            domain: request.target.clone(),
            source,
            composer: Some(composer),
            cancellation,
            width,
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
            semantics,
            scan: None,
            pending_line: None,
            top: None,
            top_lookup: None,
            target: None,
            vertical: None,
            visual: None,
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
            && self.source.utf16_len() == request.input.utf16_len
            && width(request, self.padding).is_ok_and(|width| width == self.width)
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
    ) -> Result<()> {
        self.target_source_health()?;
        self.composer
            .as_mut()
            .ok_or(Error::Closed)?
            .begin_word_group(action, head)?;
        self.target_source_health()
    }

    /// 视觉Word仅在原全源UBA明确UniformZero时可借逻辑组；准备不与GCB共用两份量子。
    pub(super) fn prepare_word_move(
        &mut self,
        owner: &Owner,
        request: &NativeMarkdownPagedSceneRequest,
    ) -> Result<Option<bool>> {
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
        Ok(step.uniform_zero)
    }

    /// 同一主队列回合只消费一份原4KiB/256量子；记录继续计入原Producer真实工作统计。
    pub(super) fn step_word_group(
        &mut self,
        owner: &Owner,
        request: &NativeMarkdownPagedSceneRequest,
    ) -> Result<omni_markdown_core::MarkdownWordGroupAnswer> {
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
        if let Some(composer) = self.composer.as_mut() {
            composer.cancel_word_group();
        }
    }
    pub(super) fn height(&self) -> f64 {
        self.height + self.padding * 2.0
    }
    pub(super) fn progress(&self) -> Result<NativeMarkdownPagedBodyProgress> {
        let work = self
            .composer
            .as_ref()
            .ok_or(Error::Closed)?
            .frontier_work()?;
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
            frontier_source_bytes: work.source_bytes,
            shaped_input_bytes: work.input_bytes,
            composer_step_bytes: self.step_bytes,
            composer_step_scalars: self.step_scalars,
            composer_step_metadata_units: self.step_metadata,
            maximum_step_bytes: self.maximum_step_bytes,
            maximum_step_scalars: self.maximum_step_scalars,
            maximum_step_metadata_units: self.maximum_step_metadata,
            trial_source_bytes: self.trial_source_bytes,
            semantic_work: self
                .semantics
                .as_ref()
                .map_or_else(Default::default, |semantic| semantic.work),
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
            target_max_batch_lines: self.target_max_batch_lines,
            target_checkpoint_attempts: self.target_checkpoint_attempts,
            target_checkpoint_millis: self.target_checkpoint_elapsed.as_secs_f64() * 1000.0,
            target_batch_index_records: self.target_batch_index_records,
        })
    }
    pub(super) fn has_in_flight(&self) -> bool {
        self.trial.as_ref().is_some_and(|trial| trial.in_flight)
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
                result.push(super::semantics::RootLeafPart {
                    source: part.facts().source_utf16.clone(),
                    points: part.facts().points,
                    native_points: fonts[index].native().point_size(),
                    role: typography.role_at(index),
                });
            }
            line = output.previous_line(&actual)?;
        }
        self.target_source_health()?;
        Ok(result)
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
                .map_or(Ok(()), |result| result.retire().map_err(Error::from))
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
        })
    }

    /// Drop only the weak directory cursor; the single Producer retirement still releases Source and reader.
    pub(super) fn cancel_visual(&mut self) {
        drop(self.visual.take());
    }

    /// Alternate visual queries with original font/line work; a directory-read turn cannot also run a Composer quantum.
    fn advance_visual(
        &mut self,
        owner: &Owner,
        request: &NativeMarkdownPagedSceneRequest,
    ) -> Result<TargetAdvance> {
        let pending = owner.pending_word_delete.borrow().clone();
        let Some(pending) = pending.filter(|pending| pending.visual_motion().is_some()) else {
            self.cancel_visual();
            return Ok(TargetAdvance::VisibleBand);
        };
        let seed = pending.visual_seed.get().ok_or(ViewError::Revoked)?;
        match self.step_visual_intent(request, seed)? {
            VisualAdvance::Ready(ready) => {
                pending.visual_ready.set(Some(ready));
                Ok(TargetAdvance::Handled)
            }
            VisualAdvance::NeedPrefix => Ok(TargetAdvance::NeedPrefix),
            VisualAdvance::Pending => {
                owner.schedule()?;
                Ok(TargetAdvance::Handled)
            }
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
        if self.pending_line.is_some() {
            self.advance_line(owner)?;
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
            self.start_style(owner, style, ticket)?;
            owner.schedule()?;
            return Ok(()); // 同一回合只推进原语义量子或Font准备，不再偷做Composer下一步。
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
            if viewport.geometry()?.content_height != self.height() {
                viewport.set_content_height(self.height())?;
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
            && self.lines != 0
            && (self.height >= needed || self.complete)
        {
            return self.publish(owner, request, ticket);
        }
        if self.complete {
            return Err(Error::Unsupported);
        }
        let composer = self.composer.as_mut().ok_or(Error::Closed)?;
        let step = composer.step(MarkdownLineComposeBudget {
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
        match step.answer {
            MarkdownLineComposeAnswer::Yielded => owner.schedule()?,
            MarkdownLineComposeAnswer::NeedStyle(request) => {
                if self.semantics.is_some() {
                    self.pending_style = Some(request);
                    owner.schedule()?; // 新语义请求在独立后继量子推进，不能把读取藏进原Composer step。
                } else {
                    self.start_style(owner, request, ticket)?;
                }
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
                let fonts = owner.fonts.borrow().clone();
                let font = fonts.first().ok_or(Error::Closed)?;
                let response = request.respond(
                    font.registered(),
                    font.native().point_size() as f32,
                    owner.paragraph.core_style().clone(),
                )?;
                composer.provide_control(response)?;
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
        }
        Ok(())
    }

    fn start_style(
        &mut self,
        owner: &Owner,
        request: MarkdownLineStyleRequest,
        ticket: &PreparationTicket,
    ) -> Result<()> {
        let mut selection = if let Some(semantic) = self.semantics.as_mut() {
            match semantic.select(owner, &self.source, &request, ticket)? {
                Some(selection) => selection,
                None => {
                    self.pending_style = Some(request);
                    return Ok(());
                }
            }
        } else {
            let points = owner.fonts.borrow()[0].native().point_size() as f32;
            super::semantics::Selection::literal(&self.source, &request, points)
        };
        selection.require(owner, &self.source, &request)?;
        let semantic_base = if self.semantics.is_some() {
            selection.base_index(owner, &self.source, ticket)?
        } else {
            0
        };
        let probe = self
            .composer
            .as_mut()
            .expect("Style阶段持唯一Composer")
            .font_probe(&request)?;
        if probe.source_range().len() > 4096 {
            return Err(Error::Unsupported);
        }
        let fonts = owner.fonts.borrow().clone();
        let continuation_font = request.continuation_font().cloned();
        let base_index = continuation_font
            .as_ref()
            .map_or(semantic_base, |authority| {
                fonts
                    .iter()
                    .position(|font| font.native().core_font().permits(authority))
                    .expect("原Frontier Font由当前Body持有")
            });
        let base = &fonts[base_index];
        // continuation只能改变Font，沿原语义项相同字号/语言/features；不能借回退重选Body样式。
        selection.style.points = base.native().point_size() as f32;
        let fallback = NativeMarkdownFontFallback::new_with_probe(
            probe.clone(),
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
        self.trial = Some(FontTrial {
            request,
            fallback: Some(fallback),
            selection,
            in_flight: false,
            result: None,
            probe,
            base_index,
            allow_cascade: self.semantics.is_some() || fonts.len() == 1,
            continuation_font,
            continuation_rejected: false,
        });
        self.advance_trial(owner, ticket)?;
        Ok(())
    }

    fn advance_trial(&mut self, owner: &Owner, ticket: &PreparationTicket) -> Result<bool> {
        let mut trial = self.trial.take().ok_or(Error::Closed)?;
        if trial.selection.context_changed(owner) || trial.fallback.is_none() {
            // 原selection已经改变：先取消旧试次，不把新publication借给旧结果。
            // 在途packet仍由同一个worker交回后析构，drain完成前不发送第二试次混淆serial。
            if let Some(fallback) = trial.fallback.take() {
                fallback.close();
            }
            if trial.in_flight {
                self.trial = Some(trial);
                return Ok(true);
            }
            let retired = trial
                .result
                .take()
                .map_or(Ok(()), |result| result.retire().map_err(Error::from));
            self.pending_style = Some(trial.request);
            retired?; // 原context已换，但真实旧试次清理失败不能被下一样式请求吞掉。
            return Ok(false);
        }
        trial
            .selection
            .require(owner, &self.source, &trial.request)?;
        if trial.in_flight {
            self.trial = Some(trial);
            return Ok(true);
        }
        let fonts = owner.fonts.borrow().clone();
        let base = fonts.get(trial.base_index).ok_or(Error::Closed)?;
        let authority = base.native().core_font().authority();
        let source = &self.source;
        #[cfg(feature = "native-markdown-runtime-probe")]
        let probe =
            owner.take_preparation_probe(&ticket.request, source, self.composer.as_ref())?;
        let current = || {
            if !ticket.current(owner, source) {
                return false;
            }
            #[cfg(feature = "native-markdown-runtime-probe")]
            super::preparation_probe::deliver(&probe);
            ticket.current(owner, source)
        };
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
                NativeMarkdownFontFallbackCompletion::Rejected { origin, .. } => {
                    if origin == crate::native_surface::paged_font_fallback::NativeMarkdownFontOrigin::Base
                        && trial.continuation_font.as_ref().is_some_and(|original| {
                            base.native().core_font().permits(original)
                        })
                    {
                        // 只有原完整试次的实际glyph0才证明旧Font不覆盖这个GCB；
                        // Frontier的类别拒绝本身不能把后继字体选择变成真实Font边界。
                        trial.continuation_rejected = true;
                    }
                    if origin == crate::native_surface::paged_font_fallback::NativeMarkdownFontOrigin::Base
                        && !trial.allow_cascade {
                        // 原活动 palette 的每个真实 capture 必须重新试原 GCB，名称、cmap
                        // 或旧文档的缺字结果都不能补签新 Source。全部拒绝后回原 base0 的级联。
                        trial.base_index += 1;
                        if trial.base_index == fonts.len() {
                            trial.base_index = 0;
                            trial.allow_cascade = true;
                        }
                        let next = &fonts[trial.base_index];
                        trial.fallback = Some(NativeMarkdownFontFallback::new_with_probe(
                            trial.probe.clone(), next.native(), &next.native().core_font().authority(),
                            NativeMarkdownProbedFontFallbackRequest {
                                style: MarkdownLineComposeStyle { points: next.native().point_size() as f32,
                                    language: None, features: Vec::new() },
                                languages: &["zh-Hans", "en"], cascade_budget: NativeMarkdownFontCascadeBudget::default(),
                                maximum_font_bytes: owner.configuration.maximum_font_bytes,
                            }, owner.mtm,
                        )?);
                    }
                    // 原完整试次确实缺字才推进原有序级联，不跳过尚未完成的候选。
                    self.trial = Some(trial);
                    owner.schedule()?;
                    return Ok(false);
                }
                NativeMarkdownFontFallbackCompletion::Selected(choice) => {
                    if let Some(original) = &trial.continuation_font {
                        let selected = choice.font(base.native(), &self.source)?;
                        if !selected.core_font().permits(original) && !trial.continuation_rejected {
                            return Err(crate::native_surface::paged_font_fallback::NativeMarkdownFontFallbackError::InvalidState.into());
                        } // 不同Font只能接在原Font真实缺字之后；同Font成功继续原plan。
                    }
                    let selected =
                        choice.into_captured_font(base.native(), &self.source, &current)?;
                    let font = if let Some(selected) = selected {
                        let selected = Rc::new(crate::native_surface::paged_line_fonts::NativeMarkdownLineFont::register(
                            selected, &owner.catalog, &current,
                        )?);
                        owner.fonts.borrow_mut().push(selected.clone());
                        selected
                    } else {
                        base.clone()
                    };
                    // 这个 style 只是原样式字体选择；首 GCB 试次不证明整个 request 无
                    // 缺字。后继原 Composer 对实际范围整形，真实 painter 缺字即拒绝候选。
                    trial
                        .selection
                        .require(owner, &self.source, &trial.request)?;
                    ticket.require(owner, &self.source)?;
                    let response = trial.request.respond_registered(
                        trial.selection.coverage,
                        font.registered(),
                        trial.selection.style,
                    )?;
                    self.composer
                        .as_mut()
                        .ok_or(Error::Closed)?
                        .provide_style(response)?;
                    return Ok(false);
                }
            }
        }
        match trial
            .fallback
            .as_mut()
            .expect("活试次持原fallback")
            .next_trial_probed(base.native(), &authority, &current)?
        {
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
            NativeMarkdownFontFallbackStep::UnavailableCandidate { .. } => {
                self.trial = Some(trial);
                owner.schedule()?;
                Ok(false)
            }
            NativeMarkdownFontFallbackStep::PendingDependencies => Err(Error::Unsupported),
            NativeMarkdownFontFallbackStep::Exhausted { .. } => Err(Error::Unsupported),
        }
    }

    fn accept_line(&mut self, owner: &Owner, facts: &MarkdownComposedLineFacts) -> Result<()> {
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
            metrics: LineMetrics::new(handle),
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
        let handle = batch.handles.pop_front().ok_or(Error::Unsupported)?;
        if handle.facts().ordinal != self.lines {
            return Err(Error::Unsupported);
        }
        self.pending_line = Some(PendingLine {
            output: batch.output.clone(),
            metrics: LineMetrics::new(handle),
            target_batch: true,
        });
        if !batch.handles.is_empty() {
            self.pending_batch = Some(batch);
        }
        self.advance_line(owner)
    }

    fn advance_line(&mut self, owner: &Owner) -> Result<()> {
        let mut line = self.pending_line.take().ok_or(Error::Closed)?;
        let mut parts = METADATA_PER_TURN;
        let before = line.metrics.part;
        let ready = line.metrics.advance(&line.output, owner, &mut parts)?;
        if line.target_batch {
            self.target_batch_index_records = self
                .target_batch_index_records
                .checked_add((line.metrics.part - before) * 4)
                .ok_or(Error::CounterExhausted)?;
        }
        if !ready {
            self.pending_line = Some(line);
            return Ok(());
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
        Ok(())
    }

    fn publish(
        &mut self,
        owner: &Owner,
        request: &NativeMarkdownPagedSceneRequest,
        ticket: &PreparationTicket,
    ) -> Result<()> {
        ticket.require(owner, &self.source)?;
        let view = owner.view()?;
        let viewport = owner.viewport.borrow().clone().ok_or(Error::Closed)?;
        let content_height = self.height();
        if viewport.geometry()?.content_height != content_height {
            viewport.set_content_height(content_height)?;
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
        let scan = match self.scan.as_mut() {
            Some(scan) => scan,
            None => self.scan.insert(Scan {
                request: request.clone(),
                cursor,
                bottom: prefix_height + self.padding,
                first_top: self.padding,
                handles: VecDeque::new(),
                ranges: VecDeque::new(),
                paragraphs: VecDeque::new(),
                simple_ltr: true,
                metrics: None,
            }),
        };
        let top = request.virtual_viewport.document_origin.y;
        let bottom = top + request.viewport.size.height;
        let mut parts = METADATA_PER_TURN;
        for _ in 0..METADATA_PER_TURN {
            if scan.metrics.is_none() {
                let Some(handle) = scan.cursor.take() else {
                    break;
                };
                scan.metrics = Some(LineMetrics::new(handle));
            }
            let mut metrics = scan.metrics.take().ok_or(Error::Closed)?;
            if !metrics.advance(&output, owner, &mut parts)? {
                scan.metrics = Some(metrics);
                owner.schedule()?;
                return Ok(());
            }
            let height = metrics.height()?;
            let ltr = metrics.ltr;
            let handle = metrics.handle;
            let line_top = scan.bottom - height;
            let previous = output.borrow_mut().previous_line(&handle)?;
            if scan.bottom >= top && line_top <= bottom {
                if scan.handles.len() == MAX_WINDOW_LINES {
                    return Err(Error::Unsupported);
                }
                scan.first_top = line_top;
                scan.simple_ltr &= ltr;
                scan.ranges.push_front(handle.facts().source_utf16.clone());
                scan.paragraphs
                    .push_front(handle.facts().paragraph_utf16.clone());
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
            owner.schedule()?;
            return Ok(());
        }
        let scan = self.scan.take().ok_or(Error::Closed)?;
        if scan.handles.is_empty() {
            return Err(Error::Unsupported);
        }
        let geometry = viewport.geometry()?;
        let nonce = NativeMarkdownPagedSceneNonce::new();
        let key = NativeMarkdownPagedSceneKey {
            request: request.clone(),
            source: self.source.authority(),
            nonce: nonce.authority(),
        };
        let palette = owner.fonts.borrow().clone();
        let first_ordinal = scan
            .handles
            .front()
            .ok_or(Error::Unsupported)?
            .facts()
            .ordinal;
        let first_top = NSPoint::new(
            self.padding,
            request.viewport.size.height + top - scan.first_top,
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
            let active_line = if owner.configuration.presentation_mode
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
                    number: (range.start == logical.1.start).then(|| logical.0 + 1),
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
        let scene = Rc::new(Scene::new(super::scene::Publication {
            key,
            nonce,
            geometry,
            window,
            output,
            owner: owner.weak.clone(),
            ranges: scan
                .ranges
                .into_iter()
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            paragraphs: scan
                .paragraphs
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
            active_line,
            gutter,
        }));
        drop(owner.scene.replace(Some(scene.clone())));
        let gutter = owner.gutter.borrow().clone();
        if let Some(gutter) = gutter {
            gutter.publish(&scene)?;
        }
        view.scene_ready()?;
        ticket.require(owner, &self.source)?;
        if self.top.is_none() && top == 0.0 {
            self.top = Some(TopPrefix {
                last_ordinal: self.lines.checked_sub(1).ok_or(Error::Unsupported)?,
                height: self.height,
            });
        }
        Ok(())
    }
}

/// 只用canonical行树定位原活动头的逻辑行；完整子树读聚合，命中叶至多4KiB，不扫描整段。
fn source_line_range_at_head(
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
                result.retire().map_err(Error::from),
            );
        }
        if !self.active.get() || self.serial.get() != serial {
            return result.retire().map_err(Error::from); // 迟到只失去字体选择权，真实资源错误仍须交给原Body。
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
                .retire()
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
        drop(self.semantics.take()); // 真正取消唯一来源请求并释放Ready，不靠旧Native任务回收。
        drop(self.composer.take());
        self.vertical = None;
        drop(self.output.take());
    }
}

pub(super) fn width(request: &NativeMarkdownPagedSceneRequest, padding: f64) -> Result<i64> {
    let points = request.bounds_width - padding * 2.0;
    let fixed = points * 64.0;
    if !fixed.is_finite() || fixed <= 0.0 || fixed >= i64::MAX as f64 {
        return Err(ViewError::InvalidGeometry.into());
    }
    Ok(fixed.round() as i64)
}

impl LineMetrics {
    fn new(handle: MarkdownComposedLine) -> Self {
        Self {
            handle,
            part: 0,
            ascent: 0.0,
            descent: 0.0,
            leading: f64::NEG_INFINITY,
            ltr: true,
        }
    }
    fn advance(
        &mut self,
        output: &RefCell<MarkdownComposedLines>,
        owner: &Owner,
        budget: &mut usize,
    ) -> Result<bool> {
        let palette = owner.fonts.borrow().clone();
        while self.part < self.handle.facts().parts && *budget != 0 {
            let part = output
                .borrow_mut()
                .part(&self.handle, self.part)?
                .ok_or(Error::Unsupported)?;
            *budget -= 1; // 只有读到实际 part 才计费，未完游标原样留给下一次延续。
            let authority = part.font_registration().authority()?;
            let font = palette
                .iter()
                .find(|font| font.native().core_font().permits(&authority))
                .ok_or(Error::Unsupported)?;
            font.require(part.font_registration())?;
            let metrics = font.metrics()?;
            self.ascent = self.ascent.max(metrics.ascent);
            self.descent = self.descent.max(metrics.descent);
            self.leading = self.leading.max(metrics.leading);
            self.ltr &= part.facts().level == 0;
            self.part += 1;
        }
        Ok(self.part == self.handle.facts().parts)
    }
    fn height(&self) -> Result<f64> {
        let height = self.ascent + self.descent + self.leading;
        if self.part != self.handle.facts().parts || !height.is_finite() || height <= 0.0 {
            return Err(ViewError::InvalidGeometry.into());
        }
        Ok(height)
    }
}

pub(super) fn scene_error(
    cause: crate::native_surface::paged_line_viewport::NativeMarkdownComposedViewportError,
) -> ViewError {
    use crate::native_surface::paged_line_viewport::NativeMarkdownComposedViewportError as Cause;
    match cause {
        Cause::Composed(omni_markdown_core::MarkdownLineComposeError::Source(cause)) => {
            ViewError::Document(cause)
        }
        Cause::Revoked => ViewError::Revoked,
        Cause::Busy => ViewError::Busy,
        // 本窗口的离屏/缺乏几何不是自动重试许可；只有 owner 另行排队真实工作才可 Pending。
        Cause::Pending => ViewError::InvalidRange,
        Cause::InvalidRange => ViewError::InvalidRange,
        other => ViewError::Raster(other.to_string()),
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
