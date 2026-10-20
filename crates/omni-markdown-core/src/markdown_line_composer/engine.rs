//! 把真实分项、唯一Store和原Measure串成可取消worker；任何公开中间状态都不冒充已布局。
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

/// LineReady只由真实LineEnd产生；待处理控制或unsafe/词典/缺字都保留原阶段。
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
    /// 本次只消费已发布目录或字形；供调度器在原剩余配额内继续，不授予新的源读取额度。
    /// 未逐项证明的阶段始终为 false，零 bytes 本身不能证明没有原文端点或控制字符读取。
    pub source_free_continuation: bool,
    /// 真实前沿子任务的邻接上下文读量；和source扫描、带halo输入分别保留。
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
/// GCB复制只服务真实Ready item/control；配额暂停保留同一个待消费owner，不重建Job。
#[derive(Clone, Copy)]
enum GraphemeResume {
    Item,
    Control,
}
struct Payload {
    source: MarkdownLayoutTextSource,
    coordinator: MarkdownLineItemCoordinator,
    // 词组删除只保原GCB上的临时游标；不持第二Source、Reader或正文副本。
    #[cfg(feature = "paged-shaping-frontier")]
    word_delete: Option<word::WordJob>,
    store: GlyphStore,
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    table: Option<table_row::PendingRow>,
    stage: Stage,
    // 真实LB Pending后仅留一个恢复标量；下一量子只推进原依赖，原Stage和consumer原样保留。
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
    // 所有selector和已移出的prefix都先真实归还，最后才结算原selector收据。
    resolver_resources: Option<super::worker_resources::ResolveResourceClaim>,
}

impl Payload {
    /// 原构造与同源样式重排共用唯一组合游标；传入的索引依赖已由调用方确定所有权。
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

/// 只保一个实际Job、一个Measure和一个固定页Store；历史item在同一匿名File中检索。
/// 字体owner由实际注册palette持有，本worker和最终行值只持同一有限catalog的弱注册。
pub struct MarkdownLineComposer {
    // 固定大续体由本worker唯一持有；取还外壳只移动指针，不反复搬运内联计划工作区。
    // 分配不随正文长度增长，原Payload字段顺序和失败时整体归还的边界保持不变。
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
    /// 显式前沿候选只交已认证prefix；原new的整item对照不在运行中切换策略。
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
    /// 原源健康始终先行；只读固定工作标量，不提前确保布局、读取正文或重签scope。
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
            } // 真实失败先撤同一深层selector位，再归还实际child及唯一File。
            drop(self.payload.take());
        } // 所有失败撤整个worker，不留下可重用的半目录或原Job。
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

    /// 仅在首次 step 之前安装原锚点的有限物理行投影；正文、Source 身份与语言索引保持原样。
    /// 隐区仍连续覆盖 canonical 坐标，但不会送入字体选择或字形整形。
    ///
    /// # Errors
    /// 超过原折叠上限、端点不是真实物理换行、已开始排版或源失败时撤销本 worker。
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
            // 输出保留同一有限端点 proof，公开 GCB 查询必须识别未迁入的隐区，而非答成非字素。
            payload.store.set_fold_plan(payload.coordinator.fold_plan());
            Ok(())
        })();
        self.finish(result)
    }
    /// 只观察原writer的实际FD/映射与源释放；快照reader必须另取自身收据。
    pub fn resource_observer(&self) -> Result<MarkdownComposedLineResourceObserver> {
        self.guard()?;
        self.payload
            .as_ref()
            .ok_or(MarkdownLineComposeError::Poisoned)?
            .store
            .resource_observer()
    }
    /// 失败或取消后仍能观察原资源事实；此getter不给正文、字体或布局任何新权限。
    pub fn worker_resource_observer(&self) -> MarkdownLineComposerWorkerResourceObserver {
        self.resources.observer()
    }

    /// 选择相关的语法显隐变化使旧字形/软行失效；同一Source和已计算语言索引继续使用。
    /// 调用前必须归还旧prefix Reader及Font试次；本方法不读取正文，也不重建UBA/Script/GCB。
    ///
    /// # Errors
    /// 原Source失败、取消、旧派生资源未归还或新输出文件创建失败时撤销本worker。
    #[cfg(feature = "paged-shaping-frontier")]
    pub fn reset_styles(&mut self, fonts: MarkdownLineFontCatalog) -> Result<()> {
        let old_resources = self.resources.observer();
        let result = (|| {
            self.guard()?;
            // 这个局部作用域先结束整张旧glyph图；不能在旧Measure/item/Store仍活时创建新writer。
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
            }; // payload在此作用域内声明，未移出的旧item/Measure等字段也在新图创建前析构。
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
            } // 真实关闭失败不能被新空writer洗掉；复用中的语言依赖在最终会话关闭时独立验收。
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
            drop(self.payload.take()); // 失败候选先真实退出，随后读取同次Drop刚记录的Source/shape首错。
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
                }); // 原操作与真实归还errno共同保留，不能用新空账本或Poisoned覆盖失败详情。
            }
            return Err(cause); // payload已移出时不能调用常规finish，把真实原错改写成“空payload”。
        }
        self.finish(Ok(()))
    }
    /// 查询当前已经发布的样式请求，不推进协调器、读取正文或重新签发 nonce。
    /// 调用方可在本次有限工作结束后保留它，下一回合仍沿原样式响应入口消费。
    ///
    /// # Errors
    /// 原 Source、取消或 worker 已失效时拒绝；样式阶段缺少原请求仍是真实覆盖错误。
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

    /// 外部响应仍消费原协调器的nonce；同范围、同名字体或新捕文档均不能补签旧请求。
    /// 只借原协调器的唯一依赖签发一个GCB，不推进排版或构造第二份索引。
    ///
    /// # Errors
    /// 原Source、阶段、请求或取消位失效时整条worker撤权。
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

    /// 原字体候选已真实归还当前 worker 后，借唯一协调器推进同一 GCB 的下一 level 段。
    /// 只转移实际目录费用，不重建 Bidi/Script 或为平台 worker 创建第二份 owner。
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

    /// 同一候选任一真实段缺字后才重启外层 GCB；原请求、取消与失败责任仍归该 Composer。
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

    /// 只借当前右GCB请求的同一RootLeaf票；不把Rc出版物放入必须Send的请求或字体worker。
    /// 原样式颜色变化不直接封口，后继实际Font响应仍由唯一协调器验证。
    ///
    /// # Errors
    /// 原Source首错、取消、阶段或nonce失效时拒绝，不能借相同数值范围重放。
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

    /// 外部响应消费原协调器的nonce；原完整字体响应仍由唯一coordinator核验。
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
                    // 原字体交接GCB已持两张收费端点票；同步响应不再查询索引或读正文。
                    payload.coordinator.provide_style(response)?;
                } else {
                    payload.coordinator.queue_style(response)?;
                }
            }
            #[cfg(not(feature = "paged-shaping-frontier"))]
            payload.coordinator.provide_style(response)?;
            if !font_witness {
                payload.style_inset_26_6 = inset;
            } // 右GCB只见证Font边界，不能把它的段落缩进写给仍待发布的左项；正常响应保持原合同。
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
                    // 原 TextKit 仍显示真实尾分隔符之后的空段；折到未终止正文 EOF 则没有后继空行。
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

    /// 每回合只推进一种有限工作；UBA本身仍是明确可取消的后台整源元数据阶段。
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
            // 上个consumer的配额已经结算；本次只做一次原LB推进，不同时重试cursor，
            // 避免同一bytes/records预算在依赖和消费两边各花一次。
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
            return Ok(result); // 同一个原GCB前进一次后交回消费者，不能同量子再读一份预算。
        }
        // 只认可已审核的原 GCB/字形目录消费阶段。Item、行起点和重排封存可能
        // 隐式读取源端点，不能把它们的零 bytes 误当作可在同一量子继续的证明。
        // 依赖推进的早返回保持 false；真实读源仍归它本来的独立步骤与错误路径。
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
                        // 样式探测消费原BreakJob，不要求把全源GCB复制到输出。只有真实
                        // Ready子item产生后，才迁入该子item实际需要的有限前缀。
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
                    // 所需GCB真实写完后才开启glyph/control capture，记录可交错但一个
                    // glyph/cluster/style段内部仍保持其原连续格式和一次producer权限。
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
                    drop(payload.item.take()); // 写完原实际glyph/cluster/style及origin后才归还Job及其FD。
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
                } else if let Some(proof) = payload.store.item_at(payload.start)? {
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
                    }); // 只有原latest LineEnd私有票可作紧急GCB新行首；普通构造仍须实际UAX机会。
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
                // 对下一已封存项执行一次元数据准入；这是完整语义操作，不是底层记录或 pread 数。
                // Tab 尚待决时仍交回原控制路径，不能因已经查过目录就获得继续合并的权限。
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
                // 同 Store 前后项已经真实交接，只读已有目录/字形；失败或提前返回均未到此签发。
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
                }; // 结束原Store借用后才查后继目录或写下一项。
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
                        } // 仅原Measure封闭safe许可可复用已测glyph，不把公开紧急Range拿去签行。
                        MarkdownSoftLineMeasureChoice::NeedNextItem {
                            source_utf16,
                            x_advance_26_6,
                            ..
                        } => {
                            payload.next_pen = x_advance_26_6;
                            if let Some(next) = payload.store.item_at(source_utf16.end)? {
                                payload.next = Some(next);
                                payload.stage = Stage::Continue;
                            } else {
                                payload.stage = Stage::Item;
                            }
                        }
                        blocked @ (MarkdownSoftLineMeasureChoice::NeedsFontFallback { .. }
                        | MarkdownSoftLineMeasureChoice::NeedsDictionary(_)) => {
                            // 缺字和词典请求还没有可供重排的字形许可。保留原 measure 的选中
                            // 状态，每次恢复仍核原 Source/Font；不能误入只接受重排的 resolver，
                            // 更不能把 InvalidRequest 当作缺字处理或伪造已完成软行。
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
                }; // LineEnd已真实写完后再签一次性行首；失败不会把半行或旧票带到下一Measure。
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
                        }; // 先核原current的真实nonce与选中范围，再归还借用查同Store旧目录。
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
                                // 只有整个选中范围属于这一旧item才可恢复；实际Font/points由该目录真实注册与样式提供。
                                measure.restore_selected_item(&mut provider, &previous_font)?;
                                payload.current = Some(previous);
                                payload.pen = 0; // 已认证Glyphs的cluster/reshape不使用pen；仅Tab依赖它，而上方目录已排除Tab。
                            } // 跨多个Font/Script的选中范围保留原NextItem能力出口，不借其中一段伪造整段。
                        }
                    }
                } // 已启动的原selector保持原锚点，不在每个量子重绑定或重新创建一个重排Job。
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
                // 重排正文与构造 halo 分开归还原量子；不能把只读了上下文的步骤当作零源工作。
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
                        // 词典、缺字或跨item重排仍需真实后继；不把当前字体重签为过去item。
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
                    } // 本路径只消费真实签包；未归还child或旧fulltail结果均不能冒充成功。
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
                } // 同一个原Measure/selector继续FIFO；不为下一行重扫或重形完整剩余tail。
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
            // 只接受原LB query留下的需求；Glyph/GCB Pending、Yielded及缺字/词典结果
            // 不会设置此位，也不改变其既有blocked路由或把未完成前缀签成EOF。
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
    /// 保留原worker并交付真实已提交行；后继滚动可继续同一cursor，不从源起点重新组合。
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
            } // 外部签包转换尚未归还child时不截候选，也不消费原selector/Measure。
            payload.store.publish_ready_prefix()
        })();
        if matches!(result, Err(MarkdownLineComposeError::SnapshotBusy)) {
            // Current与candidate都在用时只拒绝第三份FD；取消或原Source首错仍撤整个worker。
            if let Err(error) = self.guard() {
                return self.finish(Err(error));
            }
            return result;
        }
        self.finish(result)
    }

    /// 消费worker只交付已签出的前缀；没有LineEnd或仍在写item时不能构造假空行。
    pub fn into_output(mut self) -> Result<MarkdownComposedLines> {
        self.guard()?;
        let mut payload = self
            .payload
            .take()
            .ok_or(MarkdownLineComposeError::Poisoned)?;
        if payload.resolved_capture.is_some() {
            return Err(MarkdownLineComposeError::InvalidRequest);
        } // 正在转换的候选没有LineEnd，不能截断为已发布prefix。
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
    if let Some(resolver) = payload.resolver.take() {
        let observer = resolver.resource_observer();
        let resources = resolver.close()?;
        if resources.active_jobs != 0
            || resources.cleanup_failed
            || observer.snapshot().active_jobs != 0
        {
            return Err(MarkdownLineComposeError::Poisoned);
        }
    } // 原selector结算实际子job后才归还Measure；observer不保活业务owner。
    drop(payload.resolver_resources.take());
    Ok(())
}

#[cfg(test)]
#[path = "engine/lazy_graphemes_tests.rs"]
mod lazy_graphemes_tests;

#[cfg(all(test, feature = "paged-shaping-frontier"))]
#[path = "engine/line_prefix_tests.rs"]
mod line_prefix_tests;

#[cfg(all(test, feature = "paged-shaping-frontier"))]
#[path = "engine/unicode_prefix_tests.rs"]
mod unicode_prefix_tests;
