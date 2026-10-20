//! 同一worker的固定资源收据；只保存原observer和标量，不延长业务owner的生命。
//!
//! 固定依赖保留各自真实收据。临时owner的claim必须放在该owner之后，实际析构完成
//! 才结算原收据；登记数和Source释放本身都不能替代FD、页与映射的实际归还。

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use super::{MarkdownComposedLineResourceObserver, MarkdownComposedLineResources};
use crate::{
    MarkdownBidiResourceObserver, MarkdownBidiResourceSnapshot, MarkdownGlyphClusterIndex,
    MarkdownGlyphClusterResourceObserver, MarkdownGraphemeResourceSnapshot,
    MarkdownLayoutBreakResourceObserver, MarkdownLayoutTextSource,
    MarkdownLayoutTextSourceObserver, MarkdownLineBreakResourceSnapshot,
    MarkdownOriginalBackingObserver, MarkdownScriptResourceObserver,
    MarkdownScriptResourceSnapshot, MarkdownShapingIoError, MarkdownShapingJob,
    MarkdownShapingResourceObserver, MarkdownSoftLineResolveResourceObserver,
};

/// 所有已登记shape的实际最终计数；active不为零时，尚不能对其最终FD/map签释放。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MarkdownLineComposerShapeResources {
    pub registered: usize,
    pub active: usize,
    pub settled: usize,
    pub files_created: u64,
    pub files_closed: u64,
    pub mappings_created: u64,
    pub mappings_released: u64,
    pub files_open_after_drop: usize,
    pub mappings_open_after_drop: usize,
    pub mapped_bytes_after_drop: usize,
    pub work_bytes_after_drop: usize,
    /// 已结算单个Job的同时工作块峰值最大值，不把先后Job的峰值加成常驻量。
    pub peak_work_bytes_per_owner: usize,
    pub first_error: Option<MarkdownShapingIoError>,
    pub cleanup_failed: bool,
    pub access_uncertified: bool,
}

/// 临时cluster索引只在真正Drop后结算；这组标量不会积累每item的observer历史。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MarkdownLineComposerClusterResources {
    pub registered: usize,
    pub active: usize,
    pub settled: usize,
    pub files_open_after_drop: usize,
    pub mapped_bytes_after_drop: usize,
    pub buffer_bytes_after_drop: usize,
    /// 所有已结算cluster owner中最大的单owner写buffer峰值，不是各owner峰值累加。
    pub peak_buffer_bytes_per_owner: usize,
    pub cleanup_failed: bool,
}

/// 原selector的收据已经在OwnedShape析构后核实际shaper资源；这里归并整次selector。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MarkdownLineComposerResolveResources {
    pub registered: usize,
    pub active: usize,
    pub settled: usize,
    pub jobs_created: usize,
    pub jobs_released: usize,
    pub jobs_active_after_drop: usize,
    pub cleanup_failed: bool,
}

/// 原表格结构分页目录的固定角色；单个 FD 和 16KiB 页不随行数形成常驻 owner 表。
#[cfg(all(
    feature = "s2-table-window-prototype",
    feature = "paged-shaping-frontier"
))]
#[derive(Clone, Copy, Debug)]
pub struct MarkdownLineComposerTableResources {
    pub owner_released: bool,
    pub directory_created: bool,
    pub file_open: bool,
    pub mapped_bytes: usize,
    pub buffer_bytes: usize,
    pub peak_buffer_bytes: usize,
    pub cleanup_failed: bool,
    pub reader_failed: bool,
}

/// None明确表示该固定角色尚未创建；coverage_complete=false时不能签完整worker释放。
#[derive(Clone, Copy, Debug)]
pub struct MarkdownLineComposerWorkerResources {
    pub coverage_complete: bool,
    pub bidi_coverage_complete: bool,
    pub frontier_coverage_complete: bool,
    pub constructor_coverage_complete: bool,
    pub observation_failed: bool,
    pub source_payload_released: bool,
    pub first_source_errno: Option<i32>,
    pub bidi: Option<MarkdownBidiResourceSnapshot>,
    pub scripts: Option<MarkdownScriptResourceSnapshot>,
    pub graphemes: Option<MarkdownGraphemeResourceSnapshot>,
    pub line_breaks: Option<MarkdownLineBreakResourceSnapshot>,
    pub writer: Option<MarkdownComposedLineResources>,
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    pub table_directory: Option<MarkdownLineComposerTableResources>,
    pub shapes: MarkdownLineComposerShapeResources,
    pub clusters: MarkdownLineComposerClusterResources,
    pub resolvers: MarkdownLineComposerResolveResources,
}

impl MarkdownLineComposerWorkerResources {
    /// 健康完成还要求原Source从未报错；故障后真实释放不擦除这份失败事实。
    pub fn is_released(&self) -> bool {
        self.first_source_errno.is_none() && self.actual_resources_released()
    }

    /// 只判定原资源是否实际归还，供已单独认证原Source首errno的故障处理消费。
    /// 逐角色覆盖、最后Source owner、FD/map和所有清理错误仍须完整通过；不清洗任何errno。
    pub fn actual_resources_released(&self) -> bool {
        self.coverage_complete
            && !self.observation_failed
            && self.source_payload_released
            && self.bidi.is_none_or(|value| {
                !value.file_open && value.cache_bytes == 0 && value.close_errno.is_none()
            })
            && self.scripts.is_none_or(|value| {
                !value.run_file_open
                    && !value.stack_file_open
                    && value.mapped_bytes == 0
                    && value.buffer_bytes == 0
                    && !value.cleanup_failed
            })
            && self.graphemes.is_none_or(|value| {
                // 密集 ASCII 索引本来没有 FD；仍须等其原 owner 实际退场，不能以零文件签释放。
                value.metadata_owner_released
                    && !value.metadata_file_open
                    && value.metadata_map_bytes == 0
                    && value.metadata_buffer_bytes == 0
                    && !value.cleanup_failed
            })
            && self.line_breaks.is_none_or(|value| {
                !value.metadata_file_open
                    && value.metadata_map_bytes == 0
                    && value.metadata_buffer_bytes == 0
                    && !value.cleanup_failed
            })
            && self.writer.is_none_or(|value| {
                !value.file_open
                    && value.mapped_bytes == 0
                    && value.buffer_bytes == 0
                    && !value.cleanup_failed
                    && !value.reader_failed
            })
            && {
                #[cfg(all(
                    feature = "s2-table-window-prototype",
                    feature = "paged-shaping-frontier"
                ))]
                {
                    self.table_directory.is_none_or(|value| {
                        value.owner_released
                            && !value.file_open
                            && value.mapped_bytes == 0
                            && value.buffer_bytes == 0
                            && !value.cleanup_failed
                            && !value.reader_failed
                    })
                }
                #[cfg(not(all(
                    feature = "s2-table-window-prototype",
                    feature = "paged-shaping-frontier"
                )))]
                {
                    true
                }
            }
            && self.shapes.active == 0
            && self.shapes.registered == self.shapes.settled
            && self.shapes.files_created == self.shapes.files_closed
            && self.shapes.mappings_created == self.shapes.mappings_released
            && self.shapes.files_open_after_drop == 0
            && self.shapes.mappings_open_after_drop == 0
            && self.shapes.mapped_bytes_after_drop == 0
            && self.shapes.work_bytes_after_drop == 0
            && self.shapes.first_error.is_none()
            && !self.shapes.cleanup_failed
            && !self.shapes.access_uncertified
            && self.clusters.active == 0
            && self.clusters.registered == self.clusters.settled
            && self.clusters.files_open_after_drop == 0
            && self.clusters.mapped_bytes_after_drop == 0
            && self.clusters.buffer_bytes_after_drop == 0
            && !self.clusters.cleanup_failed
            && self.resolvers.active == 0
            && self.resolvers.registered == self.resolvers.settled
            && self.resolvers.jobs_created == self.resolvers.jobs_released
            && self.resolvers.jobs_active_after_drop == 0
            && !self.resolvers.cleanup_failed
    }
}

#[derive(Default)]
struct Records {
    bidi: Option<MarkdownBidiResourceObserver>,
    scripts: Option<MarkdownScriptResourceObserver>,
    breaks: Option<MarkdownLayoutBreakResourceObserver>,
    writer: Option<MarkdownComposedLineResourceObserver>,
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    table_directory: Option<crate::markdown_layout_source::table_rows::TablePlanObserver>,
    shapes: MarkdownLineComposerShapeResources,
    clusters: MarkdownLineComposerClusterResources,
    resolvers: MarkdownLineComposerResolveResources,
    frontier_pending: usize,
}
struct State {
    records: Mutex<Records>,
    failed: AtomicBool,
    // 分析发生但失败前未交出原observer时，必须保留覆盖缺口，不能把None解释成未分配。
    bidi_unobserved: AtomicBool,
    frontier_unobserved: AtomicBool,
    construction_unobserved: AtomicBool,
    source: MarkdownLayoutTextSourceObserver,
    backing: Option<MarkdownOriginalBackingObserver>,
}

/// 观察者只保固定收据和纯原子状态；持有它不会保活Source、Font、File或任务。
#[derive(Clone)]
pub struct MarkdownLineComposerWorkerResourceObserver(Arc<State>);
impl MarkdownLineComposerWorkerResourceObserver {
    /// 锁poison返回None，绝不以全零快照伪装已经归还；状态也永久记失败。
    pub fn snapshot(&self) -> Option<MarkdownLineComposerWorkerResources> {
        let Ok(records) = self.0.records.lock() else {
            self.0.failed.store(true, Ordering::Release);
            return None;
        };
        let (graphemes, line_breaks) = match &records.breaks {
            Some(value) => {
                let (graphemes, line_breaks) = value.snapshots();
                (Some(graphemes), Some(line_breaks))
            }
            None => (None, None),
        }; // 没创建的角色保留None；已创建的角色始终回读其原始实际收据。
        #[cfg(all(
            feature = "s2-table-window-prototype",
            feature = "paged-shaping-frontier"
        ))]
        let table_directory = if let Some(observer) = &records.table_directory {
            let actual = observer.snapshot();
            Some(MarkdownLineComposerTableResources {
                owner_released: actual.owner_released,
                directory_created: actual.file.is_some(),
                file_open: actual.file.is_some_and(|file| file.file_open),
                mapped_bytes: actual.file.map_or(0, |file| file.mapped_bytes),
                buffer_bytes: actual.file.map_or(0, |file| file.buffer_bytes),
                peak_buffer_bytes: actual.file.map_or(0, |file| file.peak_buffer_bytes),
                cleanup_failed: actual.file.is_some_and(|file| file.cleanup_failed),
                reader_failed: actual.file.is_some_and(|file| file.reader_failed),
            })
        } else {
            None
        };
        Some(MarkdownLineComposerWorkerResources {
            coverage_complete: !self.0.bidi_unobserved.load(Ordering::Acquire)
                && !self.0.frontier_unobserved.load(Ordering::Acquire)
                && records.frontier_pending == 0
                && !self.0.construction_unobserved.load(Ordering::Acquire),
            bidi_coverage_complete: !self.0.bidi_unobserved.load(Ordering::Acquire),
            frontier_coverage_complete: !self.0.frontier_unobserved.load(Ordering::Acquire)
                && records.frontier_pending == 0,
            constructor_coverage_complete: !self.0.construction_unobserved.load(Ordering::Acquire),
            observation_failed: self.0.failed.load(Ordering::Acquire),
            source_payload_released: self.0.source.payload_released(),
            first_source_errno: self
                .0
                .backing
                .as_ref()
                .and_then(|value| value.first_errno()),
            bidi: records.bidi.as_ref().map(|value| value.snapshot()),
            scripts: records.scripts.as_ref().map(|value| value.snapshot()),
            graphemes,
            line_breaks,
            writer: records.writer.as_ref().map(|value| value.snapshot()),
            #[cfg(all(
                feature = "s2-table-window-prototype",
                feature = "paged-shaping-frontier"
            ))]
            table_directory,
            shapes: records.shapes,
            clusters: records.clusters,
            resolvers: records.resolvers,
        })
    }

    pub fn is_released(&self) -> bool {
        self.snapshot().is_some_and(|value| value.is_released())
    }
    #[cfg(test)]
    pub(super) fn poison_for_test(&self) {
        let _guard = self.0.records.lock().unwrap();
        panic!("测试真实Mutex poison，不写资源计数或伪造FD归还");
    }
}

/// ParentScope是后继内部Job薄hook唯一入口；只共享本次worker的固定账本。
#[derive(Clone)]
pub(crate) struct ComposerResourceScope(Arc<State>);
impl ComposerResourceScope {
    pub(super) fn new(source: &MarkdownLayoutTextSource) -> Self {
        Self(Arc::new(State {
            records: Mutex::new(Records::default()),
            failed: AtomicBool::new(false),
            bidi_unobserved: AtomicBool::new(false),
            frontier_unobserved: AtomicBool::new(false),
            construction_unobserved: AtomicBool::new(false),
            source: source.resource_observer(),
            backing: source.original_backing_observer(),
        }))
    }
    pub(super) fn observer(&self) -> MarkdownLineComposerWorkerResourceObserver {
        MarkdownLineComposerWorkerResourceObserver(self.0.clone())
    }
    fn update(&self, update: impl FnOnce(&mut Records) -> Option<()>) -> bool {
        let Ok(mut records) = self.0.records.lock() else {
            self.0.failed.store(true, Ordering::Release);
            return false;
        };
        if update(&mut records).is_none() {
            self.0.failed.store(true, Ordering::Release);
            return false;
        } // 溢出/重复登记只撤观察权限，不panic、不覆盖业务Source的首I/O错误。
        true
    }
    pub(super) fn register_dependencies(
        &self,
        scripts: MarkdownScriptResourceObserver,
        breaks: MarkdownLayoutBreakResourceObserver,
    ) {
        self.update(|records| {
            if records.scripts.is_some() || records.breaks.is_some() {
                return None;
            }
            records.scripts = Some(scripts);
            records.breaks = Some(breaks);
            Some(())
        });
    }
    pub(super) fn register_writer(&self, writer: MarkdownComposedLineResourceObserver) {
        self.update(|records| {
            if records.writer.is_some() {
                return None;
            }
            records.writer = Some(writer);
            Some(())
        });
    }
    /// 与原 scripts/breaks/writer 同一固定角色账本；只允许当前结构目录登记一次实际收据。
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    pub(super) fn register_table_directory(
        &self,
        observer: crate::markdown_layout_source::table_rows::TablePlanObserver,
    ) {
        self.update(|records| {
            if records.table_directory.is_some() {
                return None;
            }
            records.table_directory = Some(observer);
            Some(())
        });
    }

    pub(super) fn begin_bidi(&self) {
        self.0.bidi_unobserved.store(true, Ordering::Release);
    }
    pub(crate) fn incomplete(&self) {
        self.0
            .construction_unobserved
            .store(true, Ordering::Release);
    }
    pub(super) fn register_bidi(&self, observer: MarkdownBidiResourceObserver) {
        if self.update(|records| {
            if records.bidi.is_some() {
                return None;
            }
            records.bidi = Some(observer);
            Some(())
        }) {
            self.0.bidi_unobserved.store(false, Ordering::Release);
        }
    }
    #[cfg(feature = "paged-shaping-frontier")]
    pub(super) fn begin_frontier_audit(&self) -> FrontierResourceRegistration {
        let registered = self.update(|records| {
            records.frontier_pending = records.frontier_pending.checked_add(1)?;
            Some(())
        });
        FrontierResourceRegistration {
            scope: self.clone(),
            registered,
            installed: false,
        }
    }
    pub(crate) fn claim_shape(
        &self,
        observer: MarkdownShapingResourceObserver,
    ) -> ShapeResourceClaim {
        let registered = self.update(|records| {
            let registered = records.shapes.registered.checked_add(1)?;
            let active = records.shapes.active.checked_add(1)?;
            records.shapes.registered = registered;
            records.shapes.active = active;
            Some(())
        });
        ShapeResourceClaim {
            scope: self.clone(),
            observer,
            registered,
        }
    }
    pub(super) fn claim_cluster(
        &self,
        observer: MarkdownGlyphClusterResourceObserver,
    ) -> ClusterResourceClaim {
        let registered = self.update(|records| {
            let registered = records.clusters.registered.checked_add(1)?;
            let active = records.clusters.active.checked_add(1)?;
            records.clusters.registered = registered;
            records.clusters.active = active;
            Some(())
        });
        ClusterResourceClaim {
            scope: self.clone(),
            observer,
            registered,
        }
    }
    pub(super) fn claim_resolver(
        &self,
        observer: MarkdownSoftLineResolveResourceObserver,
    ) -> ResolveResourceClaim {
        let registered = self.update(|records| {
            let registered = records.resolvers.registered.checked_add(1)?;
            let active = records.resolvers.active.checked_add(1)?;
            records.resolvers.registered = registered;
            records.resolvers.active = active;
            Some(())
        });
        ResolveResourceClaim {
            scope: self.clone(),
            observer,
            registered,
        }
    }
}

/// 每次真实Frontier构造前登记一次；失败Drop永久留下缺口，后继安装绝不清洗旧失败。
#[cfg(feature = "paged-shaping-frontier")]
pub(crate) struct FrontierResourceRegistration {
    scope: ComposerResourceScope,
    registered: bool,
    installed: bool,
}
#[cfg(feature = "paged-shaping-frontier")]
impl FrontierResourceRegistration {
    pub(crate) fn install(mut self) -> Option<ComposerResourceScope> {
        if !self.registered {
            return None;
        }
        if !self.scope.update(|records| {
            records.frontier_pending = records.frontier_pending.checked_sub(1)?;
            Some(())
        }) {
            return None;
        }
        self.installed = true;
        Some(self.scope.clone())
    }
}
#[cfg(feature = "paged-shaping-frontier")]
impl Drop for FrontierResourceRegistration {
    fn drop(&mut self) {
        if !self.installed {
            self.scope
                .0
                .frontier_unobserved
                .store(true, Ordering::Release);
        } // pending及缺口均不恢复；这是观察失败，不能由别的成功Frontier补证。
    }
}

/// 原Job先真实析构，后面的claim才结算；移动整个wrapper不会拆散这项顺序义务。
pub(super) struct ShapeResourceOwner {
    job: MarkdownShapingJob,
    _claim: Option<ShapeResourceClaim>,
}
impl ShapeResourceOwner {
    pub(super) fn new(job: MarkdownShapingJob, scope: &ComposerResourceScope) -> Self {
        // Frontier在实际child内已经前置登记时移交同一claim；外层不再把一个Job计两次。
        let claim = if job.has_composer_resource_scope(scope) {
            None
        } else {
            Some(scope.claim_shape(job.resource_observer()))
        };
        Self { job, _claim: claim }
    }
}
impl std::ops::Deref for ShapeResourceOwner {
    type Target = MarkdownShapingJob;
    fn deref(&self) -> &Self::Target {
        &self.job
    }
}
impl std::ops::DerefMut for ShapeResourceOwner {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.job
    }
}

/// cluster构造成功即包住实际owner；后继任何?失败也不会在索引Drop前错误结算。
pub(super) struct ClusterResourceOwner {
    index: MarkdownGlyphClusterIndex,
    _claim: Option<ClusterResourceClaim>,
}
impl ClusterResourceOwner {
    pub(super) fn new(
        index: MarkdownGlyphClusterIndex,
        scope: Option<&ComposerResourceScope>,
    ) -> Self {
        let claim = scope.map(|scope| scope.claim_cluster(index.resource_observer()));
        Self {
            index,
            _claim: claim,
        }
    }
}
impl std::ops::Deref for ClusterResourceOwner {
    type Target = MarkdownGlyphClusterIndex;
    fn deref(&self) -> &Self::Target {
        &self.index
    }
}
impl std::ops::DerefMut for ClusterResourceOwner {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.index
    }
}

/// 此claim必须在真实Job之后析构；未来内部hook也只能移交它，不能手动签“成功”。
pub(crate) struct ShapeResourceClaim {
    scope: ComposerResourceScope,
    observer: MarkdownShapingResourceObserver,
    registered: bool,
}
impl ShapeResourceClaim {
    pub(crate) fn belongs_to(&self, scope: &ComposerResourceScope) -> bool {
        Arc::ptr_eq(&self.scope.0, &scope.0)
    }
}
impl Drop for ShapeResourceClaim {
    fn drop(&mut self) {
        if !self.registered {
            return;
        }
        let actual = self.observer.snapshot();
        self.scope.update(|records| {
            let mut value = records.shapes;
            value.active = value.active.checked_sub(1)?;
            value.settled = value.settled.checked_add(1)?;
            value.files_created = value.files_created.checked_add(actual.files_created)?;
            value.files_closed = value.files_closed.checked_add(actual.files_closed)?;
            value.mappings_created = value
                .mappings_created
                .checked_add(actual.mappings_created)?;
            value.mappings_released = value
                .mappings_released
                .checked_add(actual.mappings_released)?;
            value.files_open_after_drop =
                value.files_open_after_drop.checked_add(actual.files_open)?;
            value.mappings_open_after_drop = value
                .mappings_open_after_drop
                .checked_add(actual.mappings_open)?;
            value.mapped_bytes_after_drop = value
                .mapped_bytes_after_drop
                .checked_add(actual.mapped_bytes)?;
            value.work_bytes_after_drop =
                value.work_bytes_after_drop.checked_add(actual.work_bytes)?;
            value.peak_work_bytes_per_owner =
                value.peak_work_bytes_per_owner.max(actual.peak_work_bytes);
            value.first_error = value.first_error.or(actual.first_error);
            value.cleanup_failed |= actual.cleanup_failed;
            value.access_uncertified |= !actual.shaper_access_certified;
            records.shapes = value;
            Some(())
        }); // 单Job退场不能要求共享Font/Source payload已释放；最后owner由固定源票另核。
    }
}

pub(super) struct ClusterResourceClaim {
    scope: ComposerResourceScope,
    observer: MarkdownGlyphClusterResourceObserver,
    registered: bool,
}
impl Drop for ClusterResourceClaim {
    fn drop(&mut self) {
        if !self.registered {
            return;
        }
        let actual = self.observer.snapshot();
        self.scope.update(|records| {
            let mut value = records.clusters;
            value.active = value.active.checked_sub(1)?;
            value.settled = value.settled.checked_add(1)?;
            value.files_open_after_drop = value
                .files_open_after_drop
                .checked_add(usize::from(actual.file_open))?;
            value.mapped_bytes_after_drop = value
                .mapped_bytes_after_drop
                .checked_add(actual.mapped_bytes)?;
            value.buffer_bytes_after_drop = value
                .buffer_bytes_after_drop
                .checked_add(actual.buffer_bytes)?;
            value.peak_buffer_bytes_per_owner = value
                .peak_buffer_bytes_per_owner
                .max(actual.peak_buffer_bytes);
            value.cleanup_failed |= actual.cleanup_failed;
            records.clusters = value;
            Some(())
        });
    }
}

pub(super) struct ResolveResourceClaim {
    scope: ComposerResourceScope,
    observer: MarkdownSoftLineResolveResourceObserver,
    registered: bool,
}
impl Drop for ResolveResourceClaim {
    fn drop(&mut self) {
        if !self.registered {
            return;
        }
        let actual = self.observer.snapshot();
        self.scope.update(|records| {
            let mut value = records.resolvers;
            value.active = value.active.checked_sub(1)?;
            value.settled = value.settled.checked_add(1)?;
            value.jobs_created = value.jobs_created.checked_add(actual.jobs_created)?;
            value.jobs_released = value.jobs_released.checked_add(actual.jobs_released)?;
            value.jobs_active_after_drop = value
                .jobs_active_after_drop
                .checked_add(actual.active_jobs)?;
            value.cleanup_failed |= actual.cleanup_failed;
            records.resolvers = value;
            Some(())
        });
    }
}
