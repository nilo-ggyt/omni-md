//! Keep one worker's fixed original observations and scalar resource facts
//! without extending the corresponding business owner's lifetime.
//!
//! Fixed dependencies retain their actual observations. A transient claim must
//! be declared after its resource owner so settlement follows actual destruction.
//! Registration counts or Source destruction cannot replace real FD/page/map release.

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

/// Aggregate actual terminal shape facts. Active owners still carry outstanding
/// file/mapping obligations even when their currently observed counts are zero.
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
    /// Maximum concurrent work bytes of one settled job, rather than a sum of
    /// sequential jobs' peaks incorrectly reported as retained memory.
    pub peak_work_bytes_per_owner: usize,
    pub first_error: Option<MarkdownShapingIoError>,
    pub cleanup_failed: bool,
    pub access_uncertified: bool,
}

/// Settle transient cluster indexes after actual destruction, without retaining
/// per-item observer history in this fixed scalar aggregate.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MarkdownLineComposerClusterResources {
    pub registered: usize,
    pub active: usize,
    pub settled: usize,
    pub files_open_after_drop: usize,
    pub mapped_bytes_after_drop: usize,
    pub buffer_bytes_after_drop: usize,
    /// Largest writer-buffer peak of one settled cluster owner, not a sum of
    /// peaks from owners that may have existed at different times.
    pub peak_buffer_bytes_per_owner: usize,
    pub cleanup_failed: bool,
}

/// Each selector's original observation checks real shaping resources after
/// OwnedShape destruction; aggregate that completed selector here.
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

/// Observe the original paged table-directory role through its single FD and
/// 16 KiB page, without a retained owner record for every row.
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

/// None means this fixed role was not created. Incomplete observation coverage
/// cannot certify complete worker release, even when observed values look empty.
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
    /// Healthy completion also requires no original Source failure; actual
    /// release after failure cannot erase that independently retained error fact.
    pub fn is_released(&self) -> bool {
        self.first_source_errno.is_none() && self.actual_resources_released()
    }

    /// Check actual original-resource return separately from a previously
    /// established Source errno. Complete role coverage, final Source ownership,
    /// descriptors/maps, and cleanup outcomes still apply without clearing any error.
    pub fn actual_resources_released(&self) -> bool {
        self.coverage_complete
            && !self.observation_failed
            && self.source_payload_released
            && self.bidi.is_none_or(|value| {
                // The original bidi owner must return its actual FD/cache and
                // report no failed close, rather than merely ending its task state.
                !value.file_open && value.cache_bytes == 0 && value.close_errno.is_none()
            })
            && self.scripts.is_none_or(|value| {
                // Both run and bracket-stack resources belong to script cleanup.
                !value.run_file_open
                    && !value.stack_file_open
                    && value.mapped_bytes == 0
                    && value.buffer_bytes == 0
                    && !value.cleanup_failed
            })
            && self.graphemes.is_none_or(|value| {
                // A dense ASCII index naturally has no FD. Its original metadata
                // owner must still actually end; zero files alone proves no release.
                value.metadata_owner_released
                    && !value.metadata_file_open
                    && value.metadata_map_bytes == 0
                    && value.metadata_buffer_bytes == 0
                    && !value.cleanup_failed
            })
            && self.line_breaks.is_none_or(|value| {
                // A completed line-break index must return its actual storage/cache.
                !value.metadata_file_open
                    && value.metadata_map_bytes == 0
                    && value.metadata_buffer_bytes == 0
                    && !value.cleanup_failed
            })
            && self.writer.is_none_or(|value| {
                // Output-reader ownership may outlive the writer task. Its actual
                // file/map/buffer and read/cleanup failures remain part of release.
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
                        // Even an unallocated table directory retains a real
                        // owner obligation until its original owner has ended.
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
                    // This feature configuration creates no table-directory role.
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
    // Analysis that failed before handing over its actual observer leaves a
    // coverage gap; None cannot then be reinterpreted as no allocation occurred.
    bidi_unobserved: AtomicBool,
    frontier_unobserved: AtomicBool,
    construction_unobserved: AtomicBool,
    source: MarkdownLayoutTextSourceObserver,
    backing: Option<MarkdownOriginalBackingObserver>,
}

/// Retain only fixed observations and atomic facts. This observer keeps no
/// Source, font, file, or task resource alive.
#[derive(Clone)]
pub struct MarkdownLineComposerWorkerResourceObserver(Arc<State>);
impl MarkdownLineComposerWorkerResourceObserver {
    /// Lock poisoning returns None and permanently marks observation failure,
    /// never a fabricated all-zero snapshot claiming resources already returned.
    pub fn snapshot(&self) -> Option<MarkdownLineComposerWorkerResources> {
        let Ok(records) = self.0.records.lock() else {
            self.0.failed.store(true, Ordering::Release);
            return None;
        };
        let (graphemes, line_breaks) = match &records.breaks {
            Some(value) => {
                // Read both original index observations from their shared break owner.
                let (graphemes, line_breaks) = value.snapshots();
                (Some(graphemes), Some(line_breaks))
            }
            // Preserve absence only for roles that were not registered as created.
            None => (None, None),
        };
        #[cfg(all(
            feature = "s2-table-window-prototype",
            feature = "paged-shaping-frontier"
        ))]
        let table_directory = if let Some(observer) = &records.table_directory {
            // Copy finite release facts from the same actual table-plan observer;
            // no file-presence or owner-completion value is inferred from row count.
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
            // An unregistered table-directory role remains explicitly absent.
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

    /// Accept only a valid complete observation that proves healthy actual release.
    pub fn is_released(&self) -> bool {
        self.snapshot().is_some_and(|value| value.is_released())
    }
}

/// Share only this worker's fixed resource observations with actual child jobs;
/// child claims must belong to this original scope.
#[derive(Clone)]
pub(crate) struct ComposerResourceScope(Arc<State>);
impl ComposerResourceScope {
    /// Capture weak Source/backing observations before registering child resources.
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
    /// Lend fixed observation state without granting ownership of the worker payload.
    pub(super) fn observer(&self) -> MarkdownLineComposerWorkerResourceObserver {
        MarkdownLineComposerWorkerResourceObserver(self.0.clone())
    }
    /// Apply one bounded accounting transition. Poison/overflow revokes
    /// observation authority without changing the actual source operation's result.
    fn update(&self, update: impl FnOnce(&mut Records) -> Option<()>) -> bool {
        let Ok(mut records) = self.0.records.lock() else {
            self.0.failed.store(true, Ordering::Release);
            return false;
        };
        if update(&mut records).is_none() {
            self.0.failed.store(true, Ordering::Release);
            return false;
        }
        // Overflow/duplicate registration invalidates observation; do not panic
        // or replace the business Source's original I/O failure.
        true
    }
    /// Register the original script/break observers once for this worker scope.
    pub(super) fn register_dependencies(
        &self,
        scripts: MarkdownScriptResourceObserver,
        breaks: MarkdownLayoutBreakResourceObserver,
    ) {
        self.update(|records| {
            if records.scripts.is_some() || records.breaks.is_some() {
                // Another dependency pair cannot overwrite the first pair's
                // outstanding resource obligations.
                return None;
            }
            records.scripts = Some(scripts);
            records.breaks = Some(breaks);
            Some(())
        });
    }
    /// Register the actual output writer once; later reader ownership is
    /// observed through that same receipt rather than a replacement writer value.
    pub(super) fn register_writer(&self, writer: MarkdownComposedLineResourceObserver) {
        self.update(|records| {
            if records.writer.is_some() {
                // A second writer cannot wash away the first writer's lifetime.
                return None;
            }
            records.writer = Some(writer);
            Some(())
        });
    }
    /// Register one actual table-directory observation in the same fixed role
    /// scope as scripts, breaks, and writer.
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
                // Never replace an existing directory observation with another owner.
                return None;
            }
            records.table_directory = Some(observer);
            Some(())
        });
    }

    /// Mark coverage pending before actual bidi construction may allocate resources.
    pub(super) fn begin_bidi(&self) {
        self.0.bidi_unobserved.store(true, Ordering::Release);
    }
    /// Retain a constructor observation gap permanently when allocation could
    /// not return its actual original observer.
    pub(crate) fn incomplete(&self) {
        self.0
            .construction_unobserved
            .store(true, Ordering::Release);
    }
    /// Close pending bidi coverage only after installing that actual owner's
    /// original observer successfully.
    pub(super) fn register_bidi(&self, observer: MarkdownBidiResourceObserver) {
        if self.update(|records| {
            if records.bidi.is_some() {
                // A later bidi observer cannot overwrite an already observed owner.
                return None;
            }
            records.bidi = Some(observer);
            Some(())
        }) {
            self.0.bidi_unobserved.store(false, Ordering::Release);
        }
    }
    #[cfg(feature = "paged-shaping-frontier")]
    /// Reserve observation before a real frontier constructor starts; failure
    /// to install this reservation later leaves an explicit permanent gap.
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
    /// Record an actual shaping owner and settle only after that owner is destroyed.
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
    /// Associate the real cluster observer with one active owner, not an item-count proxy.
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
    /// Retain the original selector observation until its actual owner ends.
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

/// Register before each actual frontier construction. Dropping an uninstalled
/// registration permanently preserves its gap; a later success cannot erase it.
#[cfg(feature = "paged-shaping-frontier")]
pub(crate) struct FrontierResourceRegistration {
    scope: ComposerResourceScope,
    registered: bool,
    installed: bool,
}
#[cfg(feature = "paged-shaping-frontier")]
impl FrontierResourceRegistration {
    /// Transfer this actual constructor reservation to the child after a
    /// successful install, retaining no authority from another construction.
    pub(crate) fn install(mut self) -> Option<ComposerResourceScope> {
        if !self.registered {
            // Failed reservation already revoked observation and cannot install a scope.
            return None;
        }
        if !self.scope.update(|records| {
            records.frontier_pending = records.frontier_pending.checked_sub(1)?;
            Some(())
        }) {
            // Failed accounting cannot certify this constructor as fully observed.
            return None;
        }
        self.installed = true;
        Some(self.scope.clone())
    }
}
#[cfg(feature = "paged-shaping-frontier")]
impl Drop for FrontierResourceRegistration {
    /// Keep failed/uninstalled construction visibly uncovered rather than
    /// fabricating that no child resource could have been allocated.
    fn drop(&mut self) {
        if !self.installed {
            self.scope
                .0
                .frontier_unobserved
                .store(true, Ordering::Release);
        }
        // Pending/gap facts are not restored by another successful frontier;
        // they describe this original constructor's missing observation.
    }
}

/// Drop the actual job before its following claim settles. Moving this wrapper
/// together preserves that field-destruction order.
pub(super) struct ShapeResourceOwner {
    job: MarkdownShapingJob,
    _claim: Option<ShapeResourceClaim>,
}
impl ShapeResourceOwner {
    /// Retain a real job together with its sole existing or newly acquired claim.
    pub(super) fn new(job: MarkdownShapingJob, scope: &ComposerResourceScope) -> Self {
        // A frontier child may already own its pre-construction claim. Do not
        // register that same actual job a second time in the outer wrapper.
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

/// Wrap the actual cluster owner immediately after construction. Later failure
/// cannot settle its observation before the index itself has been destroyed.
pub(super) struct ClusterResourceOwner {
    index: MarkdownGlyphClusterIndex,
    _claim: Option<ClusterResourceClaim>,
}
impl ClusterResourceOwner {
    /// Keep optional observation after its actual index in destruction order.
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

/// Destroy this claim after its real job. Child integration may transfer the
/// original claim but cannot manually certify successful resource return.
pub(crate) struct ShapeResourceClaim {
    scope: ComposerResourceScope,
    observer: MarkdownShapingResourceObserver,
    registered: bool,
}
impl ShapeResourceClaim {
    /// Compare original scope identity without inspecting or retaining the child payload.
    pub(crate) fn belongs_to(&self, scope: &ComposerResourceScope) -> bool {
        Arc::ptr_eq(&self.scope.0, &scope.0)
    }
}
impl Drop for ShapeResourceClaim {
    /// Settle from the actual original job's observation after its owner has ended.
    fn drop(&mut self) {
        if !self.registered {
            // Failed registration carries no valid accounting transition to settle.
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
        });
        // One job ending does not require shared font/Source payloads to end.
        // Their actual final owners are checked through separate original observations.
    }
}

pub(super) struct ClusterResourceClaim {
    scope: ComposerResourceScope,
    observer: MarkdownGlyphClusterResourceObserver,
    registered: bool,
}
impl Drop for ClusterResourceClaim {
    /// Merge actual final cluster storage facts without retaining its per-item observer.
    fn drop(&mut self) {
        if !self.registered {
            // Observation was already invalidated before a valid claim was installed.
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
    /// Settle actual completed selector jobs only after their resource owner has dropped.
    fn drop(&mut self) {
        if !self.registered {
            // No successfully registered owner exists for this accounting transition.
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
