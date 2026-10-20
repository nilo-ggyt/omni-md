//! 原UBA的纯Core可暂停驱动；编译器保存原循环locals，不展开重跑已改写的规则。
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

/// 一次显式step的硬预算；byte与scalar各自计费，不把UTF8窗口边缘当段落末尾。
#[derive(Clone, Copy, Debug)]
pub struct MarkdownBidiPrepareBudget {
    pub bytes: usize,
    pub scalars: usize,
    /// 每32B读/写及每个FD/页分配/flush操作各消费一个单位。
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

/// 所有计数只覆盖本次poll实际启动的操作；失败也保留已发生的工作，不报假0。
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

/// 构造阶段一次封闭的实际范围；不是可被调用方重签部分UBA结果的公开请求。
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

/// 只复制稳定原因，不保留正文或业务闭包；首次Storage错误的原值仍由首次返回完整交付。
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

/// 一个原worker内的固定Future和原Store；本值不承诺Send，不创建执行器或后台线程。
///
/// Source/取消handle先移入原worker，再在其中构造此job。所有await仅等待下一次显式step
/// 补预算；没有timer、Native waker、轮询服务或额外正文模型。Drop真实销毁整个Future，
/// 因而同时归还其中的Store、Source和固定页，观察者只保原子事实。
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
    /// 只接受完整原Source；0与EOF的byte坐标来自同一冻结树聚合，不隐读端点叶。
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

    /// 原资源scope在任何FD acquire之前即可取得；失败构造也不丢观察者。
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

    /// 折叠装配只建立显式可见段 scope；首个 level 仍须在后续原段量子内完成 UBA。
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

    /// 原 Body flow 只声明动态表格结构目录，UBA 仍按当前真实完整 B 段逐量子准备。
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

    /// 所有 Source 装配复用同一 Seed/Future 初始化图，差异只在显式显示 proof 是否存在。
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

    /// 原有界端点游标已经封闭完整 B 段和双坐标；这里不再同步读取边叶转换端点。
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

    /// 旧Snapshot/range入口本来就是同步契约；只在这里执行原有有限叶端点定位，
    /// 然后仍驱动同一个状态机。此初始化不冒充公开细byte预算的Prepare入口。
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
            return Err(failure.error()); // 后来的取消/坏预算不能洗掉已知元数据首错。
        }
        if let Err(error) = self.gate.health() {
            return self.fail(error);
        }
        if self.consumed {
            return Err(BidiError::PoisonedMetadata); // 已消费不能靠新预算重新签一份结果。
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
            return Ok(step); // Ready命中不再运行Future，但健康与原取消门仍必经。
        }
        let mut context = Context::from_waker(Waker::noop());
        let result = self
            .computation
            .as_mut()
            .ok_or(BidiError::PoisonedMetadata)?
            .as_mut()
            .poll(&mut context);
        // 外部reader的真实源失败最优先；已有metadata错误不被随后取消洗掉。
        if let Err(error) = self.gate.source_health() {
            return self.fail(error);
        }
        match result {
            Poll::Ready(Err(error)) => self.fail(error),
            Poll::Ready(Ok(ready)) => {
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
                if let Err(error) = self.gate.health() {
                    return self.fail(error);
                }
                Ok(self.gate.progress())
            }
        }
    }

    fn fail<T>(&mut self, error: BidiError) -> Result<T, BidiError> {
        self.computation.take(); // Drop原Future先归还其中所有局部Store/借用/Source，而非改空immutable。
        self.ready.take();
        if let Err(source) = self.gate.source_health() {
            self.failure = Some(Failure::capture(&source));
            return Err(source); // 原backing错误即使发生在最后Drop，也高于此前的取消或元数据失败。
        }
        if let Some(failure) = self.failure {
            return Err(failure.error());
        }
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
        Ok(ready) // Pending只返回None；没有任何部分rule或未来SCX/UBA前缀的Ready授权。
    }
    pub fn resource_observer(&self) -> MarkdownBidiResourceObserver {
        self.resources.clone()
    }

    /// 可见段 owner 只复核原任务的永久状态，不补预算、运行 Future 或转移 Ready 权限。
    #[cfg(feature = "paged-shaping-frontier")]
    pub(super) fn require_healthy(&self) -> Result<(), BidiError> {
        self.gate.source_health()?;
        if let Some(failure) = self.failure {
            return Err(failure.error());
        }
        self.gate.health()
    }
    /// 失败时同样可核上次实际工作；这是固定诊断计数，不授予Source或部分层级权限。
    pub fn last_step(&self) -> MarkdownBidiPrepareStep {
        self.gate.progress()
    }
    pub(super) fn run(mut self) -> Result<MarkdownBidiParagraph, BidiError> {
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
        // 表格目录最初可为空；未来行追加只撤其内部权限，不能为此预扫整篇或换一份 Source。
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
            None // 多段只按需定位真实B范围，不伪造已经计算的总数或建立第二段目录。
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
        // 完整 root 未证明零级时仅建立可见段 scope；禁止为了拿快路扫描隐藏 RTL 或控制正文。
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
