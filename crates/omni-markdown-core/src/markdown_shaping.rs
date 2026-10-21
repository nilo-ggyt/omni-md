//! Shape paged glyph runs from one frozen source with incremental scalar input, actual Font identity, and global UTF-16 clusters, without materializing text.
//!
//! A job describes a run already itemized by font, script, and direction. It does not infer mixed-font boundaries or treat a full run as one soft line.
//! Successful glyph output alone proves neither Native editing nor whole-product memory acceptance.

use std::{
    ops::Range,
    sync::{Arc, Weak},
};

use harfrust::{Buffer, BufferFlags, ClusterLevel, ShapeOptions};
use thiserror::Error;

use crate::{
    MARKDOWN_LAYOUT_SOURCE_BYTES, MarkdownDocumentBufferError, MarkdownLayoutFont,
    MarkdownLayoutFontAuthority, MarkdownLayoutFontResourceObserver, MarkdownLayoutTextAuthority,
    MarkdownLayoutTextSource, MarkdownLayoutTextSourceObserver,
};

mod cancellation;
#[cfg(feature = "paged-shaping-frontier")]
mod closed_item;
#[cfg(feature = "paged-shaping-frontier")]
pub(crate) use closed_item::PendingClosedShaping;
#[cfg(feature = "paged-shaping-frontier")]
pub(crate) mod frontier;
mod input;
mod options;
mod readonly;
#[cfg(feature = "paged-safe-glyph-slices")]
pub(crate) use readonly::MarkdownSpoolSliceOrigin;
#[cfg(test)]
mod tests;

pub use cancellation::MarkdownShapingCancellation;
pub use readonly::{
    MARKDOWN_SHAPED_SPOOL_STEP, MarkdownShapedSpool, MarkdownShapedSpoolAuthority,
    MarkdownShapedSpoolBuilder, MarkdownShapedSpoolError, MarkdownShapedSpoolIoError,
    MarkdownShapedSpoolIoOperation, MarkdownShapedSpoolResourceObserver,
    MarkdownShapedSpoolResources, MarkdownShapedSpoolStatus, MarkdownShapedSpoolStep,
};

pub use options::{
    MARKDOWN_SHAPING_MAX_FEATURES, MarkdownShapingDirection, MarkdownShapingFeature,
    MarkdownShapingOptions,
};

/// Charge byte/scalar dimensions independently; UTF-8 residual state survives steps without exceeding the byte quota.
#[derive(Clone, Copy, Debug)]
pub struct MarkdownShapingBudget {
    pub bytes: usize,
    pub scalars: usize,
}
impl Default for MarkdownShapingBudget {
    fn default() -> Self {
        Self {
            bytes: MARKDOWN_LAYOUT_SOURCE_BYTES,
            scalars: 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownShapingStatus {
    Ingesting,
    AwaitingShape,
    Ready,
    Cancelled,
    Poisoned,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownShapingStep {
    pub status: MarkdownShapingStatus,
    pub bytes: usize,
    pub scalars: usize,
}

/// Retain the shaper's 26.6 integer output rather than silently applying another CoreText advance-rounding policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownShapedGlyph {
    pub glyph_id: u32,
    pub source_utf16_cluster: usize,
    pub flags: u32,
    pub x_advance: i32,
    pub y_advance: i32,
    pub x_offset: i32,
    pub y_offset: i32,
}

#[derive(Debug, Error)]
pub enum MarkdownShapingError {
    #[error("字形源权限不匹配")]
    SourceMismatch,
    #[error("字形字体权限不匹配")]
    FontMismatch,
    #[error("字形源范围无效或不是标量边界")]
    InvalidRange,
    #[error("字形选项或配额无效")]
    InvalidOptions,
    #[error("字形坐标或计数溢出")]
    CoordinateOverflow,
    #[error("冻结字形源不是完整UTF8")]
    InvalidUtf8,
    #[error("字形任务尚未完成所需阶段")]
    Pending,
    #[error("字形任务已取消")]
    Cancelled,
    #[error("字形任务已永久撤权")]
    Poisoned,
    #[error("字形分页存储、操作或嵌套预算失败")]
    ShaperStorage,
    #[error("字形算法拒绝当前run：{0}")]
    Shape(#[from] harfrust::ShapeError),
    #[error("无法读取冻结字形源")]
    Source(#[from] MarkdownDocumentBufferError),
}

type Result<T> = std::result::Result<T, MarkdownShapingError>;

/// Observe counters and actual teardown facts without retaining fonts, text, paged mappings, or history.
#[derive(Clone)]
pub struct MarkdownShapingResourceObserver {
    paging: harfrust::PagingResourceObserver,
    source: MarkdownLayoutTextSourceObserver,
    font: MarkdownLayoutFontResourceObserver,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownShapingIoOperation {
    CreateFile,
    CloneFile,
    Preallocate,
    Resize,
    Map,
    Sync,
    Unmap,
    Close,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownShapingIoError {
    pub operation: MarkdownShapingIoOperation,
    pub errno: i32,
}

impl From<harfrust::PagingIoError> for MarkdownShapingIoError {
    fn from(error: harfrust::PagingIoError) -> Self {
        use MarkdownShapingIoOperation as Output;
        use harfrust::PagingOperation as Input;
        let operation = match error.operation {
            Input::CreateFile => Output::CreateFile,
            Input::CloneFile => Output::CloneFile,
            Input::Preallocate => Output::Preallocate,
            Input::Resize => Output::Resize,
            Input::Map => Output::Map,
            Input::Sync => Output::Sync,
            Input::Unmap => Output::Unmap,
            Input::Close => Output::Close,
        }; // Preserve the actual operation one-to-one, without inferring ownership from formatted error text.
        Self {
            operation,
            errno: error.errno,
        }
    }
}

/// Actual resource facts for this original job; virtual extents are not process RSS or physical footprint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownShapingResourceSnapshot {
    pub source_payload_released: bool,
    pub font_payload_released: bool,
    /// Actual allocation of fixed glyph-metadata work blocks; Source independently owns original text.
    pub work_bytes: usize,
    pub peak_work_bytes: usize,
    pub files_open: usize,
    pub mappings_open: usize,
    pub mapped_bytes: usize,
    pub peak_mapped_bytes: usize,
    pub files_created: u64,
    pub files_closed: u64,
    pub mappings_created: u64,
    pub mappings_released: u64,
    pub first_error: Option<MarkdownShapingIoError>,
    pub cleanup_failed: bool,
    pub shaper_access_certified: bool,
}

impl MarkdownShapingResourceObserver {
    #[must_use]
    pub fn snapshot(&self) -> MarkdownShapingResourceSnapshot {
        let paging = self.paging.snapshot();
        MarkdownShapingResourceSnapshot {
            source_payload_released: self.source.payload_released(),
            font_payload_released: self.font.payload_released(),
            work_bytes: paging.work_bytes,
            peak_work_bytes: paging.peak_work_bytes,
            files_open: paging.files_open,
            mappings_open: paging.mappings_open,
            mapped_bytes: paging.mapped_bytes,
            peak_mapped_bytes: paging.peak_mapped_bytes,
            files_created: paging.files_created,
            files_closed: paging.files_closed,
            mappings_created: paging.mappings_created,
            mappings_released: paging.mappings_released,
            first_error: paging.first_error.map(Into::into),
            cleanup_failed: paging.cleanup_failed,
            shaper_access_certified: paging.bounded_access_certified,
        }
    }

    /// Report release only after source/font payloads and this job's actual shaper mappings/files/work blocks are released.
    ///
    /// Other jobs may legitimately share the original Font or snapshot; this receipt cannot falsely claim those independent owners' release.
    #[must_use]
    pub fn resources_released(&self) -> bool {
        let snapshot = self.snapshot();
        // Keep the first operation I/O failure separately; it neither erases actual return success nor hides cleanup_failed.
        snapshot.source_payload_released
            && snapshot.font_payload_released
            && snapshot.files_open == 0
            && snapshot.mappings_open == 0
            && snapshot.mapped_bytes == 0
            && snapshot.work_bytes == 0
            && !snapshot.cleanup_failed
    }
}

#[derive(Clone, Copy, Debug)]
pub struct MarkdownShapingProfile {
    pub source_generation: u64,
    pub source_bytes_read: usize,
    pub source_borrow_max_bytes: usize,
    pub input_scalars: usize,
    pub glyph_count: usize,
    /// Certify only shaper paged access, excluding whole-product peaks from Native, fonts, or source mappings.
    pub shaper_access_certified: bool,
    pub shaper_work_bytes: usize,
    pub shaper_peak_work_bytes: usize,
    pub shaper_mapped_bytes: usize,
    pub shaper_peak_mapped_bytes: usize,
    pub shaper_files_open: usize,
}

struct Payload {
    // Drop actual paged arrays before returning the same frozen Source/Font; observers retain no strong resource edges.
    buffer: Buffer,
    source: MarkdownLayoutTextSource,
    font: MarkdownLayoutFont,
    // Only certified frontier children reuse an original plan; general Jobs construct plans through their existing public path.
    #[cfg(feature = "paged-shaping-frontier")]
    frontier_plan: Option<std::sync::Arc<harfrust::ShapePlan>>,
    #[cfg(feature = "paged-shaping-frontier")]
    frontier_word: Option<frontier::WordPrefixProof>,
    #[cfg(feature = "paged-shaping-frontier")]
    frontier_view: Option<frontier::local::CertifiedCenterView>,
}

/// A sole owner holds the actual stage buffer; failure/cancellation returns job resources without reviving through recaptured authority.
pub struct MarkdownShapingJob {
    // Indexes retain Weak identity only; another job with equal Source/Font/range cannot impersonate original glyph output.
    identity: Arc<()>,
    cancellation: MarkdownShapingCancellation,
    #[cfg(test)]
    cancellation_hook: Option<Arc<dyn Fn(harfrust::CooperativeCheckpoint) + Send + Sync>>,
    payload: Option<Payload>,
    source_authority: MarkdownLayoutTextAuthority,
    font_authority: MarkdownLayoutFontAuthority,
    resources: MarkdownShapingResourceObserver,
    source_utf16: Range<usize>,
    source_bytes: Range<usize>,
    input: input::Input,
    options: options::ValidatedOptions,
    status: MarkdownShapingStatus,
    source_bytes_read: usize,
    source_borrow_max: usize,
    // Drop the fixed resource-ledger claim last, after actual payload/Buffer and all other job fields are returned.
    #[cfg(feature = "paged-line-compose")]
    composer_resources: Option<crate::markdown_line_composer::worker_resources::ShapeResourceClaim>,
}

impl MarkdownShapingJob {
    #[cfg(feature = "paged-line-compose")]
    pub(crate) fn has_composer_resource_scope(
        &self,
        scope: &crate::markdown_line_composer::worker_resources::ComposerResourceScope,
    ) -> bool {
        self.composer_resources
            .as_ref()
            .is_some_and(|claim| claim.belongs_to(scope))
    }
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn attach_composer_resources(
        &mut self,
        scope: &crate::markdown_line_composer::worker_resources::ComposerResourceScope,
    ) -> Result<()> {
        self.payload
            .as_ref()
            .ok_or(MarkdownShapingError::Poisoned)?
            .source
            .health()?;
        if self.cancellation.requested() {
            return Err(MarkdownShapingError::Cancelled);
        }
        let resources = self.resources.snapshot();
        if self.composer_resources.is_some()
            || resources.files_created != 0
            || resources.mappings_created != 0
            || resources.peak_work_bytes != 0
        {
            scope.incomplete();
            return Err(MarkdownShapingError::InvalidOptions);
        } // Attach before the first glyph work block or I/O; late observation cannot claim construction/failure coverage retroactively.
        self.composer_resources = Some(scope.claim_shape(self.resources.clone()));
        Ok(())
    }
    /// Capture supplied original Source/Font identities, then validate ranges and read at most 40 bytes of actual adjacent context.
    ///
    /// Construction does not shape. After complete ingestion, callers explicitly run synchronous `shape`, which may perform
    /// the whole item's algorithm work; a single call is not evidence of bounded main-thread latency.
    pub fn new(
        source: MarkdownLayoutTextSource,
        source_authority: &MarkdownLayoutTextAuthority,
        font: MarkdownLayoutFont,
        font_authority: &MarkdownLayoutFontAuthority,
        source_utf16: Range<usize>,
        options: MarkdownShapingOptions,
    ) -> Result<Self> {
        if !source.permits(source_authority) {
            // Reject foreign source before acquiring shaper resources; equal text/generation cannot replace payload identity.
            return Err(MarkdownShapingError::SourceMismatch);
        }
        source.health()?; // Equal Weak identity cannot authorize a new glyph job after the original mapping becomes unavailable.
        if !font.permits(font_authority) {
            // Equal font names or face hashes do not identify the same original Font owner.
            return Err(MarkdownShapingError::FontMismatch);
        }
        if source_utf16.start > source_utf16.end || source_utf16.end > source.utf16_len() {
            return Err(MarkdownShapingError::InvalidRange);
        }
        // Store clusters as global UTF-16; reject overflow rather than truncating into a different valid coordinate.
        u32::try_from(source_utf16.end).map_err(|_| MarkdownShapingError::CoordinateOverflow)?;
        let start = source.byte_offset_for_utf16(source_utf16.start)?;
        let end = source.byte_offset_for_utf16(source_utf16.end)?;
        let options = options::ValidatedOptions::new(options, &font)?;
        Self::assemble(
            source,
            source_authority,
            font,
            font_authority,
            (source_utf16, start..end),
            options,
            MarkdownShapingCancellation::default(),
        )
    }

    /// Original FontProbe/item coordination consumes both certified coordinate domains without relocating distant graphemes.
    /// This is not a public range constructor; retain original Source/Font/options/extent validation.
    #[cfg(feature = "paged-line-compose")]
    pub(crate) fn from_certified_item_input(
        source: MarkdownLayoutTextSource,
        source_authority: &MarkdownLayoutTextAuthority,
        font: MarkdownLayoutFont,
        font_authority: &MarkdownLayoutFontAuthority,
        ranges: (Range<usize>, Range<usize>, Option<Range<usize>>),
        options: MarkdownShapingOptions,
        cancellation: MarkdownShapingCancellation,
    ) -> Result<Self> {
        source.health()?;
        if !source.permits(source_authority) {
            return Err(MarkdownShapingError::SourceMismatch);
        }
        if !font.permits(font_authority) {
            return Err(MarkdownShapingError::FontMismatch);
        }
        if ranges.0.start > ranges.0.end
            || ranges.0.end > source.utf16_len()
            || ranges.1.start > ranges.1.end
            || ranges.1.end > source.byte_len()
        {
            return Err(MarkdownShapingError::InvalidRange);
        }
        u32::try_from(ranges.0.end).map_err(|_| MarkdownShapingError::CoordinateOverflow)?;
        let options = options::ValidatedOptions::new(options, &font)?;
        let prepared = ranges
            .2
            .map(|bounds| input::prepare_context(&source, ranges.1.clone(), Some(bounds)))
            .transpose()?;
        let mut job = Self::assemble_with_context(
            source,
            source_authority,
            font,
            font_authority,
            (ranges.0, ranges.1, prepared),
            options,
            cancellation,
        )?;
        if job.cancellation.requested() {
            job.guard(source_authority, font_authority)?;
        } // Like new_with_cancellation, preserve actual Source/resource failure priority and release an already cancelled candidate payload.
        Ok(job)
    }

    /// Accept this module's validated options/original ranges; public construction and actual soft-line derivation share one assembly path.
    fn assemble(
        source: MarkdownLayoutTextSource,
        source_authority: &MarkdownLayoutTextAuthority,
        font: MarkdownLayoutFont,
        font_authority: &MarkdownLayoutFontAuthority,
        ranges: (Range<usize>, Range<usize>),
        options: options::ValidatedOptions,
        cancellation: MarkdownShapingCancellation,
    ) -> Result<Self> {
        Self::assemble_with_context(
            source,
            source_authority,
            font,
            font_authority,
            (ranges.0, ranges.1, None),
            options,
            cancellation,
        ) // General full Jobs and existing frontier entries retain their original synchronous bounded-context reads.
    }

    /// Keep one assembly algorithm; prepaid closed-item context and original synchronous reads differ only in where context is acquired.
    fn assemble_with_context(
        source: MarkdownLayoutTextSource,
        source_authority: &MarkdownLayoutTextAuthority,
        font: MarkdownLayoutFont,
        font_authority: &MarkdownLayoutFontAuthority,
        ranges: (Range<usize>, Range<usize>, Option<input::PreparedContext>),
        options: options::ValidatedOptions,
        cancellation: MarkdownShapingCancellation,
    ) -> Result<Self> {
        let (source_utf16, source_bytes, prepared) = ranges; // Both coordinate domains remain from one original ticket, without relocation.
        let mut buffer = Buffer::new();
        buffer.set_direction(options.direction);
        buffer.set_script(Some(options.script));
        buffer.set_language(options.language.clone());
        buffer.set_cluster_level(ClusterLevel::MonotoneGraphemes);
        // Edge flags come only from itemized actual text boundaries; finite steps/chunks never create new BOT/EOT.
        let mut flags = BufferFlags::PRODUCE_UNSAFE_TO_CONCAT;
        if options.beginning_of_text {
            flags |= BufferFlags::BEGINNING_OF_TEXT;
        }
        if options.end_of_text {
            flags |= BufferFlags::END_OF_TEXT;
        }
        buffer.set_flags(flags);
        let (context_bytes, context_max) = if let Some(prepared) = prepared {
            prepared.install(&mut buffer)
        } else {
            input::install_context(&source, source_bytes.clone(), &mut buffer)?
        }; // Closed-item context was already read in its previous input step; installation neither reborrows Source nor charges it twice.
        let resources = MarkdownShapingResourceObserver {
            paging: buffer.paging_resource_observer(),
            source: source.resource_observer(),
            font: font.resource_observer(),
        };
        let status = if source_utf16.is_empty() {
            MarkdownShapingStatus::AwaitingShape
        } else {
            MarkdownShapingStatus::Ingesting
        };
        Ok(Self {
            identity: Arc::new(()),
            cancellation,
            #[cfg(test)]
            cancellation_hook: None,
            payload: Some(Payload {
                buffer,
                source,
                font,
                #[cfg(feature = "paged-shaping-frontier")]
                frontier_plan: None,
                #[cfg(feature = "paged-shaping-frontier")]
                frontier_word: None,
                #[cfg(feature = "paged-shaping-frontier")]
                frontier_view: None,
            }),
            source_authority: source_authority.clone(),
            font_authority: font_authority.clone(),
            resources,
            input: input::Input::new(source_bytes.start, source_utf16.start),
            source_utf16,
            source_bytes,
            options,
            status,
            source_bytes_read: context_bytes,
            source_borrow_max: context_max,
            #[cfg(feature = "paged-line-compose")]
            composer_resources: None,
        })
    }

    /// Only the original Ready job derives its actual item subrange with original Font/options, without accepting caller-made replacement options.
    #[cfg(feature = "paged-soft-line-resolve")]
    pub(crate) fn reshape_subrange(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        range: Range<usize>,
    ) -> Result<Self> {
        let profile = self.profile()?;
        self.glyph(source, font, profile.glyph_count)?; // Actual EOF validates readiness and original authority, beyond reading local status.
        let public_range = self.source_range();
        if range.start < public_range.start
            || range.end > public_range.end
            || range.start > range.end
        {
            return Err(MarkdownShapingError::InvalidRange);
        }
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownShapingError::Poisoned)?;
        let start = payload.source.byte_offset_for_utf16(range.start)?;
        let end = payload.source.byte_offset_for_utf16(range.end)?;
        let mut options = self.options.clone();
        // A soft-line edge is not another paragraph. Only matching actual item endpoints inherit BOT/EOT; interiors retain
        // adjacent same-Source context without turning a 4-KiB window into a paragraph or changing global language/feature ranges.
        options.beginning_of_text &= range.start == self.source_utf16.start;
        options.end_of_text &= range.end == self.source_utf16.end;
        let source_value = payload.source.clone();
        let font_value = payload.font.clone();
        let cancellation = self.cancellation.clone();
        self.guard(source, font)?;
        let mut child = Self::assemble(
            source_value,
            source,
            font_value,
            font,
            (range, start..end),
            options,
            cancellation,
        )?;
        child.guard(source, font)?; // Actual source failure or original cancellation during construction cannot publish a runnable child.
        self.guard(source, font)?;
        Ok(child)
    }

    /// Existing synthetic controls place a deterministic barrier at actual deep checkpoints; product builds neither expose this entry nor retain its closure.
    #[cfg(all(test, feature = "paged-soft-line-resolve"))]
    pub(crate) fn resolve_test_checkpoint(
        &mut self,
        hook: Arc<dyn Fn(harfrust::CooperativeCheckpoint) + Send + Sync>,
    ) {
        self.cancellation_hook = Some(hook);
    }

    /// Transfer pure Core inputs across threads before the worker constructs its original job with the existing cancellation bit.
    ///
    /// Do not transfer an already constructed non-Send paged Job; cancellation retains no text/Font edges. Original new reports
    /// actual Source construction failures first, then the same guard releases an already cancelled candidate without publishing it runnable.
    pub fn new_with_cancellation(
        source: MarkdownLayoutTextSource,
        source_authority: &MarkdownLayoutTextAuthority,
        font: MarkdownLayoutFont,
        font_authority: &MarkdownLayoutFontAuthority,
        source_utf16: Range<usize>,
        options: MarkdownShapingOptions,
        cancellation: MarkdownShapingCancellation,
    ) -> Result<Self> {
        let mut job = Self::new(
            source,
            source_authority,
            font,
            font_authority,
            source_utf16,
            options,
        )?;
        job.cancellation = cancellation;
        if job.cancellation.requested() {
            job.guard(source_authority, font_authority)?; // Original I/O/Source failure wins; clean cancellation actually takes/drops candidate payload.
        }
        Ok(job) // Later original step/shape/read checks the same permanent bit; no reset window separates this check and delivery.
    }

    fn guard(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
    ) -> Result<()> {
        match self.status {
            MarkdownShapingStatus::Cancelled => return Err(MarkdownShapingError::Cancelled),
            MarkdownShapingStatus::Poisoned => return Err(MarkdownShapingError::Poisoned),
            // Every later step checks original payload, without recovering an invalid job by capturing a newer source.
            _ => {}
        }
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownShapingError::Poisoned)?;
        if let Err(error) = payload.source.health() {
            // Actual reclaim failure from another shared reader revokes this job too; cached glyphs cannot renew readiness.
            return self.poison(error.into());
        }
        if !payload.source.permits(source) || !payload.source.permits(&self.source_authority) {
            return self.poison(MarkdownShapingError::SourceMismatch);
        }
        if !payload.font.permits(font) || !payload.font.permits(&self.font_authority) {
            return self.poison(MarkdownShapingError::FontMismatch);
        }
        if !payload.buffer.allocation_successful() {
            // Shape success does not establish paging, maximum-operation, or nested-budget success; validate actual storage separately.
            return self.poison(MarkdownShapingError::ShaperStorage);
        }
        #[cfg(feature = "paged-shaping-frontier")]
        if let Some(view) = &payload.frontier_view
            && let Err(error) = view.require(self)
        {
            return self.poison(error);
        } // Support coverage is no arbitrary public clipping API; the concrete center ticket must still belong to this original Job/plan owner.
        if self.cancellation.requested() {
            return self.cancelled(); // Check Source/paging failures first so cancellation cannot hide their actual cause.
        }
        Ok(())
    }

    fn poison<T>(&mut self, error: MarkdownShapingError) -> Result<T> {
        self.status = MarkdownShapingStatus::Poisoned;
        // End the internal mutable borrow with take, then drop actual arrays/context and this job's Source/Font strong edges.
        drop(self.payload.take());
        Err(error)
    }

    /// Advance original run input; unknown suffixes remain Ingesting and only the actual input-range end permits AwaitingShape.
    pub fn step(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        budget: MarkdownShapingBudget,
    ) -> Result<MarkdownShapingStep> {
        self.guard(source, font)?;
        if budget.bytes == 0 || budget.scalars == 0 {
            return self.poison(MarkdownShapingError::InvalidOptions);
        }
        if self.status != MarkdownShapingStatus::Ingesting {
            // A zero-input observation after ingestion neither shapes again nor appends duplicate characters.
            return Ok(MarkdownShapingStep {
                status: self.status,
                bytes: 0,
                scalars: 0,
            });
        }
        let result = self.step_input(budget);
        match result {
            Ok(step) => Ok(step),
            // Retire the whole job even after a finite buffer accepted part of this step; partial input cannot publish success.
            Err(MarkdownShapingError::Cancelled) => self.cancelled(),
            Err(error) => self.poison(error),
        }
    }

    /// Synchronously shape the original complete run, separately validating I/O, actual paged-access certification, and output metadata.
    pub fn shape(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
    ) -> Result<MarkdownShapingStatus> {
        self.guard(source, font)?;
        match self.status {
            MarkdownShapingStatus::Ingesting => return Err(MarkdownShapingError::Pending),
            MarkdownShapingStatus::Ready => return Ok(MarkdownShapingStatus::Ready),
            MarkdownShapingStatus::AwaitingShape => {}
            // guard handles these terminal exits; explicit matching prevents another state from accidentally entering shape later.
            MarkdownShapingStatus::Cancelled => return Err(MarkdownShapingError::Cancelled),
            MarkdownShapingStatus::Poisoned => return Err(MarkdownShapingError::Poisoned),
        }
        let control = self.cooperative_control()?;
        let result = {
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownShapingError::Poisoned)?;
            let shaper = payload
                .font
                .shaper_at_scale(self.options.scale)
                .map_err(|_| MarkdownShapingError::InvalidOptions)?;
            // Cold Font OnceLock initialization is outside cooperative cancellation scope; afterward the original deep algorithm checks requests in batches.
            harfrust::with_cooperative_control(control, || {
                let shape_options = ShapeOptions::new()
                    .point_size(Some(self.options.points))
                    .features(&self.options.features);
                // Metadata certification and execution must use the same compiled plan rather than silently rebuilding one during shape.
                #[cfg(feature = "paged-shaping-frontier")]
                let shape_options = shape_options.plan(payload.frontier_plan.as_deref());
                harfrust::shape(&shaper, &mut payload.buffer, shape_options)
            })
        };
        if let Err(error) = result {
            // Actual Source/paging failure precedes cancellation; this terminal bridge cannot rewrite I/O failure as clean cancellation.
            let payload = self
                .payload
                .as_ref()
                .ok_or(MarkdownShapingError::Poisoned)?;
            if let Err(source_error) = payload.source.health() {
                return self.poison(source_error.into());
            }
            if payload.buffer.paging_error().is_some() {
                return self.poison(MarkdownShapingError::ShaperStorage);
            }
            if error == harfrust::ShapeError::Cancelled {
                return self.cancelled();
            }
            return self.poison(error.into());
        }
        let buffer = &self
            .payload
            .as_ref()
            .ok_or(MarkdownShapingError::Poisoned)?
            .buffer;
        if !buffer.allocation_successful() || !buffer.paged_checkpoint() {
            return self.poison(MarkdownShapingError::ShaperStorage);
        }
        self.guard(source, font)?; // Recheck original request after actual shape/paging, withholding Ready if cancellation arrived meanwhile.
        // Linux can compute glyphs but has no verified residency contract here; profile must retain that distinction.
        self.status = MarkdownShapingStatus::Ready;
        Ok(self.status)
    }

    /// Deliver one Copy glyph for actual viewport queries, without lending an entire array or document text.
    pub fn glyph(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        index: usize,
    ) -> Result<Option<MarkdownShapedGlyph>> {
        self.guard(source, font)?;
        if self.status != MarkdownShapingStatus::Ready {
            return Err(MarkdownShapingError::Pending);
        }
        let buffer = &self
            .payload
            .as_ref()
            .ok_or(MarkdownShapingError::Poisoned)?
            .buffer;
        #[cfg(feature = "paged-shaping-frontier")]
        let view = self
            .payload
            .as_ref()
            .and_then(|p| p.frontier_view.as_ref())
            .map(|v| v.glyphs.clone());
        #[cfg(not(feature = "paged-shaping-frontier"))]
        let view: Option<Range<usize>> = None;
        let count = view.as_ref().map_or(buffer.len(), Range::len);
        if index >= count {
            return Ok(None); // Actual EOF has no record; this is not temporary emptiness in an unknown input prefix.
        }
        let actual_index = view.as_ref().map_or(index, |range| range.start + index);
        let Some((info, position)) = buffer.paged_glyph_record(actual_index) else {
            return self.poison(MarkdownShapingError::ShaperStorage);
        };
        if !buffer.allocation_successful() {
            return self.poison(MarkdownShapingError::ShaperStorage);
        }
        let cluster = info.cluster as usize;
        if !self.source_range().contains(&cluster) {
            // Clusters must belong to this run's original global range; missing-glyph zero remains valid output and cannot be discarded.
            return self.poison(MarkdownShapingError::InvalidRange);
        }
        self.guard(source, font)?; // After retrieving the Copy record, require original Source/Font/token before delivering it.
        Ok(Some(MarkdownShapedGlyph {
            glyph_id: info.glyph_id,
            source_utf16_cluster: cluster,
            flags: info.flags().to_bits() & harfrust::GlyphFlags::ALL.to_bits(),
            x_advance: self.options.quantize(position.x_advance),
            y_advance: self.options.quantize(position.y_advance),
            x_offset: self.options.quantize(position.x_offset),
            y_offset: self.options.quantize(position.y_offset),
        }))
    }

    #[must_use]
    pub fn status(&self) -> MarkdownShapingStatus {
        self.status
    }
    /// Identify this actual job weakly without retaining arrays/fonts/text; derived Core indexes still check original job health.
    pub(crate) fn index_identity(&self) -> Weak<()> {
        Arc::downgrade(&self.identity)
    }
    /// Return original validated direction without promoting unfinished results to Ready.
    #[must_use]
    pub fn direction(&self) -> MarkdownShapingDirection {
        match self.options.direction {
            harfrust::Direction::LeftToRight => MarkdownShapingDirection::LeftToRight,
            harfrust::Direction::RightToLeft => MarkdownShapingDirection::RightToLeft,
            harfrust::Direction::TopToBottom => MarkdownShapingDirection::TopToBottom,
            harfrust::Direction::BottomToTop => MarkdownShapingDirection::BottomToTop,
            // Original construction admits only four concrete directions, never Invalid.
            harfrust::Direction::Invalid => unreachable!("字形任务方向已在构造时校验"),
        }
    }
    /// Return actual requested size from validated options as a style fact, not job readiness or current authority.
    #[must_use]
    pub fn points(&self) -> f32 {
        self.options.points
    }
    #[must_use]
    pub fn source_range(&self) -> Range<usize> {
        #[cfg(feature = "paged-shaping-frontier")]
        if let Some(view) = self.payload.as_ref().and_then(|p| p.frontier_view.as_ref()) {
            return view.source.clone();
        }
        self.source_utf16.clone()
    }
    #[must_use]
    pub fn resource_observer(&self) -> MarkdownShapingResourceObserver {
        self.resources.clone()
    }

    /// Obtain profile from the same live job; failure/cancellation cannot turn zeroed resources into newly successful statistics.
    pub fn profile(&mut self) -> Result<MarkdownShapingProfile> {
        let source = self.source_authority.clone();
        let font = self.font_authority.clone();
        self.guard(&source, &font)?;
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownShapingError::Poisoned)?;
        let paging = self.resources.paging.snapshot();
        let glyph_count = if self.status == MarkdownShapingStatus::Ready {
            #[cfg(feature = "paged-shaping-frontier")]
            let ready_count = payload
                .frontier_view
                .as_ref()
                .map_or_else(|| payload.buffer.len(), |view| view.glyphs.len());
            #[cfg(not(feature = "paged-shaping-frontier"))]
            let ready_count = payload.buffer.len();
            ready_count
        } else {
            0 // Before shape, Buffer length counts input scalars rather than final glyphs.
        };
        Ok(MarkdownShapingProfile {
            source_generation: payload.source.generation(),
            source_bytes_read: self.source_bytes_read,
            source_borrow_max_bytes: self.source_borrow_max,
            input_scalars: self.input.scalars,
            glyph_count,
            shaper_access_certified: payload.buffer.paged_access_certified()
                && paging.bounded_access_certified,
            shaper_mapped_bytes: paging.mapped_bytes,
            shaper_work_bytes: paging.work_bytes,
            shaper_peak_work_bytes: paging.peak_work_bytes,
            shaper_peak_mapped_bytes: paging.peak_mapped_bytes,
            shaper_files_open: paging.files_open,
        })
    }

    /// Explicit cancellation is terminal and returns this job's resources; independent observers then check actual close/unmap outcomes.
    pub fn cancel(&mut self) {
        self.cancellation.request(); // The same bit reaches any in-flight deep algorithm without creating another token.
        if matches!(
            self.status,
            MarkdownShapingStatus::Cancelled | MarkdownShapingStatus::Poisoned
        ) {
            return; // Preserve terminal failure facts rather than clearing Poisoned into clean Cancelled.
        }
        let _ = self.cancelled::<()>(); // Preserve the void API; cleanup failure remains in Poisoned state and original resource receipts.
    }
}
