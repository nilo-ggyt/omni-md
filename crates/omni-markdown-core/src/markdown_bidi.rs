//! Paged UBA metadata reads properties through bounded pieces and stores
//! paragraph-length intermediate state in anonymous files.
//!
//! X/W/N/I and retained-format-character rules derive from unicode-bidi 0.3.18.
//! This adapter uses its paged owners rather than a whole-text BidiInfo value.
//! Glyphs, caret geometry, Native objects, and viewport completion belong to
//! their later owning stages.
//!
//! Copyright 2015 The Servo Project Developers.
//! Algorithm portions adapted from unicode-bidi 0.3.18, <https://github.com/servo/unicode-bidi>.
//! SPDX-License-Identifier: MIT OR Apache-2.0
//! Preserve the upstream MIT license and attribution in this directory's
//! LICENSE-MIT alongside the paged storage/cursor adaptation.

use std::ops::Range;
use std::sync::Arc;

use thiserror::Error;
use unicode_bidi::{BidiClass, BidiDataSource};

use crate::{MarkdownDocumentBufferError, MarkdownTextSnapshot};

mod cancellation;
pub use cancellation::MarkdownBidiCancellation;
mod explicit;
mod implicit;
mod line;
mod resource;
pub use resource::{MarkdownBidiResourceObserver, MarkdownBidiResourceSnapshot};
mod prepare;
mod sequences;
mod source;
mod store;
// Document-leaf aggregates and UBA share this authoritative Unicode 18 delta;
// generated data and its original license remain at their existing location.
use crate::unicode_bidi_data as unicode_data;
mod uniform;
#[cfg(feature = "paged-shaping-frontier")]
mod visible;
pub use prepare::{
    MarkdownBidiPrepareBudget, MarkdownBidiPrepareJob, MarkdownBidiPrepareStatus,
    MarkdownBidiPrepareStep,
};
#[cfg(feature = "paged-shaping-frontier")]
pub(crate) use visible::VisiblePreparation as MarkdownBidiVisiblePreparation;

use store::{NONE, Store, Table, read_u32, write_u32};
use unicode_data::UnicodeBidiData;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BidiBaseDirection {
    Auto,
    LeftToRight,
    RightToLeft,
}

impl BidiBaseDirection {
    /// Auto defers base selection to actual paragraph rules; explicit directions
    /// supply only the corresponding original paragraph embedding level.
    fn level(self) -> Option<u8> {
        match self {
            Self::Auto => None,
            Self::LeftToRight => Some(0),
            Self::RightToLeft => Some(1),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct BidiMemoryBudget {
    pages: u16,
}

impl BidiMemoryBudget {
    pub fn new(cache_bytes: usize) -> Result<Self, BidiError> {
        // Require the caller's exact admissible cache limit. Never enlarge it
        // implicitly or replace bounded pages with a whole-table allocation.
        if !(4 * store::PAGE_BYTES..=1024 * 1024).contains(&cache_bytes)
            || !cache_bytes.is_multiple_of(store::PAGE_BYTES)
        {
            return Err(BidiError::InvalidBudget);
        }
        Ok(Self {
            pages: (cache_bytes / store::PAGE_BYTES) as u16,
        })
    }
}

impl Default for BidiMemoryBudget {
    fn default() -> Self {
        Self { pages: 64 }
    }
}

#[derive(Debug, Error)]
pub enum BidiError {
    #[error("双向文本分析已取消")]
    Cancelled,
    #[error("双向文本分析不属于原布局源")]
    SourceMismatch,
    #[error("双向文本分析范围无效")]
    InvalidRange,
    #[error("折叠内部没有可见双向文本查询权限")]
    FoldedRange,
    #[error("独立表格行内部须查询该格显示来源的 UBA 结果")]
    TableRange,
    #[error("实际双向文本段尚未在原量子内准备完成")]
    UnpreparedRange,
    #[error("双向文本分析端点不是完整标量边界")]
    InvalidScalarBoundary,
    #[error("双向文本分析缓存预算无效")]
    InvalidBudget,
    #[error("双向文本分析坐标溢出")]
    CoordinateOverflow,
    #[error("双向文本元数据不完整")]
    CorruptMetadata,
    #[error("匿名双向文本元数据已因存储失败撤销")]
    PoisonedMetadata,
    #[error("无法读取冻结正文")]
    Source(#[from] MarkdownDocumentBufferError),
    #[error("无法访问匿名双向文本元数据")]
    Storage(#[from] std::io::Error),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownBidiRun {
    pub source_utf16_start: usize,
    pub source_utf16_end: usize,
    pub level: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownBidiStorageProfile {
    pub cache_bytes: usize,
    pub page_loads: u64,
    pub source_borrow_max_bytes: usize,
    pub utf16_units: usize,
    pub unicode_version: (u64, u64, u64),
}

/// Completed scopes retain immutable logical results. Mutable queries serve
/// bounded metadata I/O and explicitly prepared paragraph scopes, with no
/// document mutation authority.
pub struct MarkdownBidiParagraph {
    // Retain the original immutable source for this analysis. Completed paged
    // lookups use derived metadata; prepared-scope operations retain their own
    // explicit bounded source-read contract.
    _snapshot: MarkdownTextSnapshot,
    // Layout capture retains the same capsule identity; snapshot-only public
    // analysis grants no independently captured layout authority.
    #[cfg(feature = "paged-layout-breaks")]
    layout_source: Option<crate::MarkdownLayoutTextSource>,
    source: Range<usize>,
    resolution: Resolution,
    units: u32,
    paragraph_count: Option<u32>,
    source_borrow_max: usize,
    // This cursor queries completed original UBA metadata and retires with its
    // actual paragraph, retaining no third paragraph or in-flight preparation.
    #[cfg(feature = "paged-shaping-frontier")]
    word_order: visible::WordOrder,
}

/// One completed UBA UTF-16 record supports actual shaping-item consumption;
/// grapheme/caret authority still requires its own proof. Byte coordinates come
/// from original Text capture without rescanning the document prefix.
#[cfg(feature = "paged-line-compose")]
#[derive(Clone)]
pub(crate) struct MarkdownBidiShapingUnit {
    source: crate::MarkdownLayoutTextAuthority,
    pub(crate) utf16: usize,
    pub(crate) byte: usize,
    pub(crate) byte_end: usize,
    pub(crate) level: u8,
    pub(crate) continuation: bool,
}

#[cfg(feature = "paged-line-compose")]
impl MarkdownBidiShapingUnit {
    /// Adjacent original UTF-16 records with a real right scalar head can mark
    /// a shaping-level seam only when they share Source and differ in level.
    pub(crate) fn precedes_different_level(&self, right: &Self) -> bool {
        self.source == right.source
            && self.utf16.checked_add(1) == Some(right.utf16)
            && self.byte_end == right.byte
            && !right.continuation
            && self.level != right.level
    }

    pub(crate) fn permits(&self, source: &crate::MarkdownLayoutTextAuthority) -> bool {
        &self.source == source
    }
}

/// Borrow completed original uniform facts without granting grapheme endpoints,
/// live-document mutation, or authority over a later Source capture.
#[cfg(feature = "paged-shaping-frontier")]
pub(crate) struct UniformControlFreeSpan<'a> {
    _paragraph: &'a MarkdownBidiParagraph,
    range: Range<usize>,
    paragraph_range: Range<usize>,
    content_end: usize,
}

#[cfg(feature = "paged-shaping-frontier")]
impl UniformControlFreeSpan<'_> {
    pub(crate) fn source_range(&self) -> Range<usize> {
        self.range.clone()
    }

    /// Return this actual analyzed paragraph's extent rather than treating the
    /// requested subrange as a new paragraph. Same-Source/cancellation checks do
    /// not prove scalar/GCB endpoints or ownership outside the analysis scope.
    pub(crate) fn paragraph_range(&self) -> Range<usize> {
        self.paragraph_range.clone()
    }

    /// The bounded same-Source B query established this content endpoint;
    /// separators still need actual consumption by original GCB/Script owners.
    pub(crate) fn content_end(&self) -> usize {
        self.content_end
    }

    pub(crate) fn level(&self) -> u8 {
        0 // Construction requires original UniformZero and complete proof that
        // this span has none of the six Markdown inline separator controls.
    }
}

/// Only a complete uniform certificate can omit paged tables. Other input keeps
/// the actual X/W/N/I algorithm and its owned metadata representation.
enum Resolution {
    UniformZero(uniform::UniformResolution),
    Paged(Store),
    #[cfg(feature = "paged-shaping-frontier")]
    VisibleParagraphs(Box<visible::VisibleParagraphs>),
}

impl Resolution {
    fn healthy(&self) -> Result<(), BidiError> {
        match self {
            Self::UniformZero(cancellation) => cancellation.check(),
            Self::Paged(store) => store.healthy(),
            #[cfg(feature = "paged-shaping-frontier")]
            Self::VisibleParagraphs(visible) => visible.healthy(),
        } // Uniform owns no file/page; its original cancellation still revokes
        // the completed analysis permanently.
    }

    #[cfg(test)]
    fn paged_mut(&mut self) -> &mut Store {
        match self {
            Self::Paged(store) => store,
            Self::UniformZero(_) => panic!("此故障/资源测试必须使用真实复杂UBA输入"),
            #[cfg(feature = "paged-shaping-frontier")]
            Self::VisibleParagraphs(_) => panic!("此故障测试必须选定原实际段的分页存储"),
        }
    }
}

impl MarkdownBidiParagraph {
    /// Tell the coordinator whether endpoint preparation exists, without
    /// reading Source or issuing proof for any numeric range.
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn uses_prepared_paragraphs(&self) -> bool {
        matches!(&self.resolution, Resolution::VisibleParagraphs(_))
            || matches!(&self.resolution, Resolution::UniformZero(certificate) if certificate.paragraphs)
    }

    /// Finite oracles count actual public paragraph ranges when no total
    /// directory exists, rather than manufacturing an aggregate count.
    #[cfg(test)]
    fn actual_paragraph_count(&mut self) -> usize {
        if let Some(count) = self.paragraph_count {
            return count as usize;
        }
        let mut position = self.source.start;
        let mut count = 0;
        while position < self.source.end {
            let paragraph = self.paragraph_range_at_utf16(position).unwrap().unwrap();
            assert_eq!(paragraph.start, position);
            assert!(paragraph.end > position);
            position = paragraph.end;
            count += 1;
        }
        assert_eq!(position, self.source.end);
        count
    }

    /// Observe actual descriptor/fixed-cache facts without retaining analysis,
    /// Source, or Native objects and without claiming current kernel RSS.
    #[must_use]
    pub fn resource_observer(&self) -> MarkdownBidiResourceObserver {
        match &self.resolution {
            Resolution::UniformZero(_) => MarkdownBidiResourceObserver::unallocated(),
            Resolution::Paged(store) => store.resource_observer(),
            #[cfg(feature = "paged-shaping-frontier")]
            Resolution::VisibleParagraphs(visible) => visible.resource_observer(),
        }
    }

    pub fn analyze(
        snapshot: MarkdownTextSnapshot,
        source: Range<usize>,
        base: BidiBaseDirection,
        budget: BidiMemoryBudget,
    ) -> Result<Self, BidiError> {
        Self::analyze_cancellable(
            snapshot,
            source,
            base,
            budget,
            &MarkdownBidiCancellation::new(),
        )
    }

    fn analyze_cancellable(
        snapshot: MarkdownTextSnapshot,
        source: Range<usize>,
        base: BidiBaseDirection,
        budget: BidiMemoryBudget,
        cancellation: &MarkdownBidiCancellation,
    ) -> Result<Self, BidiError> {
        Self::analyze_cancellable_observed(snapshot, source, base, budget, cancellation, None)
    }

    fn analyze_cancellable_observed(
        snapshot: MarkdownTextSnapshot,
        source: Range<usize>,
        base: BidiBaseDirection,
        budget: BidiMemoryBudget,
        cancellation: &MarkdownBidiCancellation,
        resources: Option<&Arc<resource::ResourceState>>,
    ) -> Result<Self, BidiError> {
        prepare::MarkdownBidiPrepareJob::snapshot_range(
            snapshot,
            source,
            base,
            budget,
            cancellation,
            resources,
        )?
        .run()
    }

    /// Use the same frozen Source as break/grapheme/shaping stages, preserving
    /// capture identity independently from equal text or generation.
    ///
    /// This synchronous entry completes the requested analysis using the original
    /// Uniform proof or actual paged state as needed. UI callers need the existing worker;
    /// Source/I/O consistency alone does not establish Native acceptance.
    #[cfg(feature = "paged-layout-breaks")]
    pub fn analyze_layout_source(
        source: crate::MarkdownLayoutTextSource,
        authority: &crate::MarkdownLayoutTextAuthority,
        paragraph: Range<usize>,
        base: BidiBaseDirection,
        budget: BidiMemoryBudget,
    ) -> Result<Self, BidiError> {
        Self::analyze_layout_source_cancellable(
            source,
            authority,
            paragraph,
            base,
            budget,
            &MarkdownBidiCancellation::new(),
        )
    }

    /// Compute synchronously under original frozen-source authority on the
    /// caller's worker. Fixed metadata accesses share cancellation; completed
    /// queries remain revoked after that original operation is cancelled.
    #[cfg(feature = "paged-layout-breaks")]
    pub fn analyze_layout_source_cancellable(
        source: crate::MarkdownLayoutTextSource,
        authority: &crate::MarkdownLayoutTextAuthority,
        paragraph: Range<usize>,
        base: BidiBaseDirection,
        budget: BidiMemoryBudget,
        cancellation: &MarkdownBidiCancellation,
    ) -> Result<Self, BidiError> {
        Self::analyze_layout_source_cancellable_observed_inner(
            source,
            authority,
            paragraph,
            base,
            budget,
            cancellation,
            None,
        )
    }

    /// Failure-path tests observe the same actual descriptor/page receipts;
    /// those observations issue neither layout nor Source authority.
    #[cfg(all(test, feature = "paged-line-compose"))]
    pub(crate) fn analyze_layout_source_cancellable_observed(
        source: crate::MarkdownLayoutTextSource,
        authority: &crate::MarkdownLayoutTextAuthority,
        paragraph: Range<usize>,
        base: BidiBaseDirection,
        budget: BidiMemoryBudget,
        cancellation: &MarkdownBidiCancellation,
    ) -> (Result<Self, BidiError>, MarkdownBidiResourceObserver) {
        let resources = resource::ResourceState::new();
        let observer = resources.observe();
        let result = Self::analyze_layout_source_cancellable_observed_inner(
            source,
            authority,
            paragraph,
            base,
            budget,
            cancellation,
            Some(&resources),
        );
        (result, observer) // Local failure cleanup has ended; the observer owns
        // no File, Source, or algorithm working state.
    }

    #[cfg(feature = "paged-layout-breaks")]
    fn analyze_layout_source_cancellable_observed_inner(
        source: crate::MarkdownLayoutTextSource,
        authority: &crate::MarkdownLayoutTextAuthority,
        paragraph: Range<usize>,
        base: BidiBaseDirection,
        budget: BidiMemoryBudget,
        cancellation: &MarkdownBidiCancellation,
        resources: Option<&Arc<resource::ResourceState>>,
    ) -> Result<Self, BidiError> {
        source.health()?;
        if !source.permits(authority) {
            return Err(BidiError::SourceMismatch); // Equal text/generation or a
            // fresh ticket cannot replace the original Source allocation.
        }
        cancellation.check()?;
        let mut analyzed = Self::analyze_cancellable_observed(
            source.snapshot_for_bidi(),
            paragraph,
            base,
            budget,
            cancellation,
            resources,
        )?;
        source.health()?;
        analyzed.resolution.healthy()?;
        analyzed.layout_source = Some(source);
        Ok(analyzed)
    }

    /// Check original Source before Native geometry publication without
    /// granting live-document or successor-generation consumption authority.
    #[cfg(feature = "paged-layout-breaks")]
    pub fn require_layout_source(
        &self,
        authority: &crate::MarkdownLayoutTextAuthority,
    ) -> Result<(), BidiError> {
        self._snapshot.original_backing_health()?; // Actual Source failure
        // precedes foreign tickets, cancellation, and completed-cache results.
        let result = (|| {
            let source = self
                .layout_source
                .as_ref()
                .ok_or(BidiError::SourceMismatch)?;
            source.health()?;
            if !source.permits(authority) {
                // Distinct captures cannot share this geometry publication
                // merely because their text and generations are equal.
                return Err(BidiError::SourceMismatch);
            }
            self.resolution.healthy()
        })();
        self._snapshot.original_backing_health()?;
        if let Some(source) = &self.layout_source {
            source.health()?;
        }
        result?; // Source revalidation preserves a concurrently established
        // actual errno before a weaker ticket/cancellation failure.
        self.resolution.healthy()
    }

    /// Only whole-original-Source UniformZero equates visual and logical order;
    /// one zero-level subparagraph cannot issue that whole-source permission.
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn whole_source_uniform_zero(
        &self,
        authority: &crate::MarkdownLayoutTextAuthority,
    ) -> Result<bool, BidiError> {
        self.require_layout_source(authority)?;
        let source = self
            .layout_source
            .as_ref()
            .ok_or(BidiError::SourceMismatch)?;
        let proved = self.source == (0..source.utf16_len())
            && matches!(self.resolution, Resolution::UniformZero(_));
        self.require_layout_source(authority)?; // Completed values still pass
        // original Source, same-capture, and cancellation checks after lookup.
        Ok(proved)
    }

    /// Identify preparation of actual visible B paragraphs, without extending
    /// local uniformity, unprepared scopes, or hidden text to whole-Source proof.
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn uses_visible_paragraph_scopes(
        &self,
        authority: &crate::MarkdownLayoutTextAuthority,
    ) -> Result<bool, BidiError> {
        self.require_layout_source(authority)?;
        Ok(matches!(self.resolution, Resolution::VisibleParagraphs(_)))
    }

    /// Borrow completed level-zero/no-Markdown-inline-control facts within the
    /// original analysis scope. Numeric ranges can include low surrogates, so
    /// actual item delivery still checks scalar and GCB endpoints separately.
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn uniform_control_free_span(
        &self,
        authority: &crate::MarkdownLayoutTextAuthority,
        range: Range<usize>,
    ) -> Result<Option<UniformControlFreeSpan<'_>>, BidiError> {
        self.require_layout_source(authority)?;
        let valid = range.start <= range.end
            && range.start >= self.source.start
            && range.end <= self.source.end;
        if let Resolution::VisibleParagraphs(visible) = &self.resolution {
            if !valid {
                self.require_layout_source(authority)?;
                return Err(BidiError::InvalidRange);
            }
            // A child certificate belongs only to its complete actual B scope;
            // it cannot establish level zero for the outer whole Source.
            return visible
                .range(&range)?
                .uniform_control_free_span(authority, range);
        }
        let paragraph_range = match &self.resolution {
            Resolution::UniformZero(certificate) if certificate.control_free() => {
                Some((self.source.clone(), self.source.end))
            }
            Resolution::UniformZero(certificate)
                if certificate.paragraphs && certificate.interior_controls_absent =>
            {
                certificate
                    .paragraph
                    .control_free(&range)
                    .map(|(paragraph, content_end)| {
                        // The frozen cursor found a complete B paragraph, but
                        // public analysis may begin within it. Intersect actual
                        // analysis bounds so Script/ascii_suffix cannot adopt
                        // outside snapshot context as its own paragraph extent.
                        (
                            paragraph.start.max(self.source.start)
                                ..paragraph.end.min(self.source.end),
                            content_end.min(self.source.end),
                        )
                    })
            }
            _ => None, // Paged or unproven control input keeps its original
                       // query path; level zero grants no GCB/font permission.
        };
        self.require_layout_source(authority)?;
        if !valid {
            return Err(BidiError::InvalidRange);
        } // Invalid numeric ranges still pass post-lookup Source/cancellation
        // checks, without borrowing document text for endpoint validation.
        Ok(
            paragraph_range.map(|(paragraph_range, content_end)| UniformControlFreeSpan {
                _paragraph: self,
                range,
                paragraph_range,
                content_end,
            }),
        ) // Borrow the actual Paragraph without cloning Source/authority or
        // transferring its certificate through equal generation numbers.
    }

    /// Paged already has its full table; multiparagraph Uniform prepares only
    /// the requested actual B scope under the existing finite work allowance.
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn prepare_uniform_paragraph_range(
        &mut self,
        authority: &crate::MarkdownLayoutTextAuthority,
        position: usize,
        budget: MarkdownBidiPrepareBudget,
    ) -> Result<uniform::ParagraphPrepareStep, BidiError> {
        self.require_layout_source(authority)?;
        if budget.bytes == 0 || budget.scalars == 0 || budget.metadata_ops == 0 {
            return Err(BidiError::InvalidBudget);
        }
        if position < self.source.start || position > self.source.end {
            return Err(BidiError::InvalidRange);
        }
        let result = match &mut self.resolution {
            Resolution::VisibleParagraphs(_) => Err(BidiError::UnpreparedRange),
            Resolution::UniformZero(certificate)
                if certificate.paragraphs && position < self.source.end =>
            {
                certificate
                    .paragraph
                    .step(&self._snapshot, position, budget)
            }
            _ => Ok(uniform::ParagraphPrepareStep::ready()),
        };
        if let Ok(step) = &result {
            self.source_borrow_max = self.source_borrow_max.max(step.maximum_borrow);
        }
        self.require_layout_source(authority)?; // Metadata pruning still retains
        // original Source/cancellation; a real lease failure precedes the result.
        result
    }

    /// The original item owns in-flight folded-scope work. Transfer only an
    /// actual completed B paragraph into the transferable sealed result.
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn prepare_layout_paragraph_range(
        &mut self,
        authority: &crate::MarkdownLayoutTextAuthority,
        position: usize,
        budget: MarkdownBidiPrepareBudget,
        work: &mut MarkdownBidiVisiblePreparation,
    ) -> Result<uniform::ParagraphPrepareStep, BidiError> {
        self.require_layout_source(authority)?;
        if budget.bytes == 0 || budget.scalars == 0 || budget.metadata_ops == 0 {
            return Err(BidiError::InvalidBudget);
        }
        if position < self.source.start || position > self.source.end {
            return Err(BidiError::InvalidRange);
        }
        let Resolution::VisibleParagraphs(visible) = &mut self.resolution else {
            return self.prepare_uniform_paragraph_range(authority, position, budget);
        };
        let result = if position == self.source.end {
            Ok(uniform::ParagraphPrepareStep::ready())
        } else {
            visible.step(
                self.layout_source
                    .as_ref()
                    .ok_or(BidiError::SourceMismatch)?,
                &self._snapshot,
                authority,
                position,
                budget,
                work,
            )
        };
        if let Ok(step) = &result {
            self.source_borrow_max = self.source_borrow_max.max(step.maximum_borrow);
        }
        if let Err(error) = &result {
            visible.remember_failure(error);
        }
        self.require_layout_source(authority)?;
        result
    }

    /// Synchronous public lookup reuses the bounded-leaf algorithm. Production
    /// ItemStart explicitly prepares first instead of hiding scans in its turn.
    #[cfg(feature = "paged-shaping-frontier")]
    fn uniform_paragraph_at(&mut self, position: usize) -> Result<Option<Range<usize>>, BidiError> {
        self._snapshot.original_backing_health()?;
        self.resolution.healthy()?;
        if position < self.source.start || position > self.source.end {
            return Err(BidiError::InvalidRange);
        }
        if position == self.source.end {
            return Ok(None);
        }
        let Resolution::UniformZero(certificate) = &mut self.resolution else {
            unreachable!()
        };
        if let Some(range) = certificate.paragraph.range_covering(position) {
            if certificate.paragraph.range_at(position).is_none()
                && !self._snapshot.utf16_boundaries_are_scalar()?
            {
                self._snapshot
                    .byte_offset_for_utf16(position)
                    .map_err(scalar_endpoint_error)?;
            } // Public queries still reject surrogate interiors; cached range
            // facts cannot substitute for an actual scalar boundary.
            self._snapshot.original_backing_health()?;
            self.resolution.healthy()?;
            // Partial analysis may start inside a real snapshot B paragraph;
            // return only the intersection actually owned by this analysis.
            return Ok(Some(
                range.start.max(self.source.start)..range.end.min(self.source.end),
            ));
        }
        while certificate.paragraph.range_at(position).is_none() {
            let step = certificate
                .paragraph
                .step(
                    &self._snapshot,
                    position,
                    MarkdownBidiPrepareBudget {
                        bytes: 4096,
                        scalars: 256,
                        metadata_ops: 256,
                    },
                )
                .map_err(|cause| match cause {
                    // Initial positioning and cached-range hits use the same
                    // public scalar error, preserving actual file Source causes.
                    BidiError::Source(cause) => scalar_endpoint_error(cause),
                    other => other,
                })?;
            self.source_borrow_max = self.source_borrow_max.max(step.maximum_borrow);
        } // Synchronous lookup may complete its metadata query without adding a
        // whole-paragraph buffer, another reader, or duplicated paragraph rules.
        let range = certificate.paragraph.range_at(position);
        self._snapshot.original_backing_health()?;
        self.resolution.healthy()?;
        Ok(range.map(|range| range.start.max(self.source.start)..range.end.min(self.source.end)))
    }

    pub fn source_range(&self) -> Range<usize> {
        self.source.clone()
    }

    /// Paged queries binary-search completed metadata. Multi-paragraph Uniform
    /// queries may complete real endpoint preparation through bounded reads;
    /// formal itemization explicitly prepares that paragraph scope beforehand.
    /// Return the actual scope within this analysis; callers separately establish
    /// outside text boundaries for partial analyses.
    /// Exact EOF is None; surrogate interiors and outside positions fail while
    /// retaining the same original Source/cancellation checks.
    pub fn paragraph_range_at_utf16(
        &mut self,
        offset: usize,
    ) -> Result<Option<Range<usize>>, BidiError> {
        #[cfg(feature = "paged-shaping-frontier")]
        if matches!(&self.resolution, Resolution::UniformZero(certificate) if certificate.paragraphs)
        {
            return self.uniform_paragraph_at(offset);
        }
        let relative = self.checked_range(offset..offset)?.start;
        if relative == self.units {
            return Ok(None);
        } // EOF has no character and cannot manufacture a trailing paragraph or
        // transfer the previous paragraph's range to a following item.
        let found = match &mut self.resolution {
            Resolution::UniformZero(_) => self.source.clone(),
            // A complete single-paragraph uniform certificate supplies this
            // analysis's exact paragraph scope for a nonempty character query.
            Resolution::Paged(store) => {
                let mut low = 0;
                let mut high = self.paragraph_count.expect("实际分页段表已经计算总数");
                let paragraph = loop {
                    if low >= high {
                        return Err(BidiError::CorruptMetadata);
                    } // Complete analysis covers every character; missing
                    // ownership is corruption, not permission to rescan text.
                    let mid = low + (high - low) / 2;
                    let paragraph = Paragraph::read(store, mid)?;
                    if paragraph.start >= paragraph.end || paragraph.end > self.units {
                        return Err(BidiError::CorruptMetadata);
                    } // Validate real nonempty disk intervals before they can
                    // drive binary-search decisions or coordinate arithmetic.
                    if relative < paragraph.start {
                        high = mid;
                    } else if relative >= paragraph.end {
                        low = mid + 1;
                    } else {
                        break paragraph;
                    } // Match the original character's half-open paragraph;
                    // separators retain captured ownership, including CRLF.
                };
                self.source.start + paragraph.start as usize
                    ..self.source.start + paragraph.end as usize
            }
            #[cfg(feature = "paged-shaping-frontier")]
            Resolution::VisibleParagraphs(visible) => visible
                .character_mut(offset)?
                .paragraph_range_at_utf16(offset)?
                .ok_or(BidiError::CorruptMetadata)?,
        };
        self._snapshot.original_backing_health()?;
        self.resolution.healthy()?; // Recheck original Source/cancellation after
        // metadata I/O so revoked candidates cannot leave this boundary.
        Ok(Some(found))
    }

    /// Read the actual paragraph base independently of its first resolved
    /// character level. Empty visual lines after LS retain that paragraph; EOF
    /// is None and a true new empty paragraph remains with its caller's policy.
    pub fn paragraph_base_level_at_utf16(
        &mut self,
        offset: usize,
    ) -> Result<Option<u8>, BidiError> {
        #[cfg(feature = "paged-shaping-frontier")]
        if matches!(&self.resolution, Resolution::UniformZero(certificate) if certificate.paragraphs)
        {
            return self
                .uniform_paragraph_at(offset)
                .map(|range| range.map(|_| 0));
        }
        let relative = self.checked_range(offset..offset)?.start;
        if relative == self.units {
            return Ok(None);
        }
        let base = match &mut self.resolution {
            Resolution::UniformZero(_) => 0, // The complete certificate proves
            // base zero, independently of glyph direction.
            Resolution::Paged(store) => {
                let (mut low, mut high) =
                    (0, self.paragraph_count.expect("实际分页段表已经计算总数"));
                loop {
                    if low >= high {
                        return Err(BidiError::CorruptMetadata);
                    }
                    let middle = low + (high - low) / 2;
                    let paragraph = Paragraph::read(store, middle)?;
                    if paragraph.start >= paragraph.end
                        || paragraph.end > self.units
                        || paragraph.base > 1
                    {
                        return Err(BidiError::CorruptMetadata);
                    }
                    if relative < paragraph.start {
                        high = middle;
                    } else if relative >= paragraph.end {
                        low = middle + 1;
                    } else {
                        break paragraph.base;
                    }
                }
            }
            #[cfg(feature = "paged-shaping-frontier")]
            Resolution::VisibleParagraphs(visible) => visible
                .character_mut(offset)?
                .paragraph_base_level_at_utf16(offset)?
                .ok_or(BidiError::CorruptMetadata)?,
        };
        self._snapshot.original_backing_health()?;
        self.resolution.healthy()?;
        Ok(Some(base))
    }

    pub fn level_at_utf16(&mut self, index: usize) -> Result<u8, BidiError> {
        self._snapshot.original_backing_health()?; // Cached levels retain the
        // original Source's permanent read failure.
        self.resolution.healthy()?;
        let relative = index
            .checked_sub(self.source.start)
            .ok_or(BidiError::InvalidRange)?;
        if relative >= self.units as usize {
            return Err(BidiError::InvalidRange); // EOF has no character level;
            // low surrogates use their actual duplicate record instead.
        }
        let level = match &mut self.resolution {
            // Per-UTF-16 levels include the low surrogate's same zero level,
            // without granting that code unit a legal run endpoint.
            Resolution::UniformZero(_) => 0,
            Resolution::Paged(store) => Text::read(store, relative as u32)?.level,
            #[cfg(feature = "paged-shaping-frontier")]
            Resolution::VisibleParagraphs(visible) => {
                visible.character_mut(index)?.level_at_utf16(index)?
            }
        };
        self._snapshot.original_backing_health()?;
        self.resolution.healthy()?;
        Ok(level)
    }

    /// Read actual level, scalar-head, and byte facts from one General UBA Text
    /// record. Uniform has no such record and returns None to its existing
    /// aggregate-based zero-level item path.
    #[cfg(feature = "paged-line-compose")]
    pub(crate) fn shaping_unit_at_utf16(
        &mut self,
        authority: &crate::MarkdownLayoutTextAuthority,
        index: usize,
    ) -> Result<Option<MarkdownBidiShapingUnit>, BidiError> {
        self.require_layout_source(authority)?;
        let relative = index
            .checked_sub(self.source.start)
            .ok_or(BidiError::InvalidRange)?;
        if relative >= self.units as usize {
            return Err(BidiError::InvalidRange);
        }
        let source_bytes = self
            .layout_source
            .as_ref()
            .expect("原 require_layout_source 已确认本次真实布局来源")
            .byte_len();
        let value = match &mut self.resolution {
            Resolution::UniformZero(_) => None,
            Resolution::Paged(store) => {
                let value = Text::read(store, relative as u32)?;
                let byte = value.byte as usize;
                // Byte width comes from this original UTF-8 capture, without
                // estimating from UTF-16 length or reading a later paragraph.
                let byte_end = byte
                    .checked_add(usize::from((value.flags >> 4) & 3) + 1)
                    .ok_or(BidiError::CoordinateOverflow)?;
                if byte_end > source_bytes {
                    return Err(BidiError::CorruptMetadata);
                }
                Some(MarkdownBidiShapingUnit {
                    source: authority.clone(),
                    utf16: index,
                    byte,
                    byte_end,
                    level: value.level,
                    continuation: value.continuation(),
                })
            }
            #[cfg(feature = "paged-shaping-frontier")]
            Resolution::VisibleParagraphs(visible) => visible
                .character_mut(index)?
                .shaping_unit_at_utf16(authority, index)?,
        };
        self.require_layout_source(authority)?; // Preserve original Source
        // failure and cancellation again after metadata I/O.
        Ok(value)
    }

    /// Visit Copy logical-run candidates in order; commit the batch only after
    /// this complete call returns Ok.
    ///
    /// I/O can fail after earlier callbacks. Err invalidates every candidate
    /// from this call, so callbacks must not publish display state, send Native
    /// actions, or certify coverage while the batch is still running.
    pub fn visit_logical_runs(
        &mut self,
        range: Range<usize>,
        mut visit: impl FnMut(MarkdownBidiRun),
    ) -> Result<(), BidiError> {
        let relative = self.checked_range(range)?;
        let store = match &mut self.resolution {
            Resolution::UniformZero(cancellation) => {
                if !relative.is_empty() {
                    // This completed scope proves continuous zero-level input;
                    // post-callback Source/cancellation still gate publication.
                    visit(MarkdownBidiRun {
                        source_utf16_start: self.source.start + relative.start as usize,
                        source_utf16_end: self.source.start + relative.end as usize,
                        level: 0,
                    });
                }
                self._snapshot.original_backing_health()?;
                return cancellation.check();
            }
            Resolution::Paged(store) => store,
            #[cfg(feature = "paged-shaping-frontier")]
            Resolution::VisibleParagraphs(visible) => {
                let range = self.source.start + relative.start as usize
                    ..self.source.start + relative.end as usize;
                let result = if range.is_empty() && range.start == self.source.end {
                    Ok(())
                } else {
                    visible.range_mut(&range)?.visit_logical_runs(range, visit)
                };
                self._snapshot.original_backing_health()?;
                self.resolution.healthy()?;
                return result;
            }
        };
        let mut at = relative.start;
        while at < relative.end {
            let level = Text::read(store, at)?.level;
            let from = at;
            while at < relative.end && Text::read(store, at)?.level == level {
                at += Text::read(store, at)?.scalar_len()?;
            }
            visit(MarkdownBidiRun {
                source_utf16_start: self.source.start + from as usize,
                source_utf16_end: self.source.start + at as usize,
                level,
            }); // The page borrow ended; callbacks receive finite Copy facts
            // without a snapshot or metadata-cache buffer.
        }
        self._snapshot.original_backing_health()?;
        self.resolution.healthy()
    }

    /// Apply L1/L2 only to an actual already-broken soft line supplied by the
    /// caller; font widths and line formation remain their own stages.
    ///
    /// Callbacks still receive candidates: later I/O failure invalidates the
    /// entire batch. Only full Ok grants this line's coverage, before which
    /// Native geometry and paint state must remain unpublished.
    pub fn visit_visual_runs(
        &mut self,
        actual_soft_line: Range<usize>,
        mut visit: impl FnMut(MarkdownBidiRun),
    ) -> Result<(), BidiError> {
        #[cfg(feature = "paged-shaping-frontier")]
        if !actual_soft_line.is_empty()
            && matches!(&self.resolution, Resolution::UniformZero(certificate) if certificate.paragraphs)
            && self
                .uniform_paragraph_at(actual_soft_line.start)?
                .is_none_or(|paragraph| actual_soft_line.end > paragraph.end)
        {
            return Err(BidiError::InvalidRange); // A real L1 line cannot cross
            // another B paragraph, including separately owned CR/LF separators.
        }
        let relative = self.checked_range(actual_soft_line)?;
        match &mut self.resolution {
            Resolution::UniformZero(_) => {
                if !relative.is_empty() {
                    // An original Uniform certificate may cover multiple B
                    // paragraphs. The actual-paragraph check above confines this
                    // soft line to one validated paragraph scope.
                    visit(MarkdownBidiRun {
                        source_utf16_start: self.source.start + relative.start as usize,
                        source_utf16_end: self.source.start + relative.end as usize,
                        level: 0,
                    });
                }
            }
            Resolution::Paged(store) => {
                line::visit(
                    store,
                    self.paragraph_count.expect("实际分页段表已经计算总数"),
                    self.source.start,
                    relative,
                    visit,
                )?;
            }
            #[cfg(feature = "paged-shaping-frontier")]
            Resolution::VisibleParagraphs(visible) => {
                let range = self.source.start + relative.start as usize
                    ..self.source.start + relative.end as usize;
                if !(range.is_empty() && range.start == self.source.end) {
                    visible.range_mut(&range)?.visit_visual_runs(range, visit)?;
                }
            }
        }
        self._snapshot.original_backing_health()?;
        self.resolution.healthy()
    }

    pub fn storage_profile(&self) -> MarkdownBidiStorageProfile {
        let (cache_bytes, page_loads) = match &self.resolution {
            Resolution::UniformZero(_) => (0, 0),
            Resolution::Paged(store) => (store.cache_bytes(), store.page_loads),
            #[cfg(feature = "paged-shaping-frontier")]
            Resolution::VisibleParagraphs(visible) => visible.storage(),
        }; // Report the actual representation; Uniform owns no hypothetical
        // cache merely because a nonzero paged budget was configured.
        MarkdownBidiStorageProfile {
            cache_bytes,
            page_loads,
            source_borrow_max_bytes: self.source_borrow_max,
            utf16_units: self.units as usize,
            unicode_version: unicode_data::UNICODE_VERSION,
        }
    }

    fn checked_range(&mut self, range: Range<usize>) -> Result<Range<u32>, BidiError> {
        self._snapshot.original_backing_health()?; // Empty lines and index hits
        // retain actual backing failure just like nonempty metadata reads.
        self.resolution.healthy()?; // Empty requests cannot revive revoked analysis.
        if range.start > range.end || range.start < self.source.start || range.end > self.source.end
        {
            return Err(BidiError::InvalidRange);
        }
        let start = (range.start - self.source.start) as u32;
        let end = (range.end - self.source.start) as u32;
        match &mut self.resolution {
            Resolution::UniformZero(_) => {
                #[cfg(feature = "paged-shaping-frontier")]
                let endpoints_are_scalar = self._snapshot.utf16_boundaries_are_scalar()?;
                #[cfg(not(feature = "paged-shaping-frontier"))]
                let endpoints_are_scalar = false;
                for position in [start, end] {
                    if endpoints_are_scalar {
                        continue;
                    } // The same root's exact no-surrogate aggregate already
                    // establishes these integer scalar endpoints.
                    // Other endpoints borrow their bounded frozen tree leaf,
                    // without adding a surrogate mirror for Uniform results.
                    self._snapshot
                        .byte_offset_for_utf16(self.source.start + position as usize)
                        .map_err(scalar_endpoint_error)?;
                }
                self._snapshot.original_backing_health()?;
                self.resolution.healthy()?;
            }
            Resolution::Paged(store) => {
                for position in [start, end] {
                    if position < self.units && Text::read(store, position)?.continuation() {
                        return Err(BidiError::InvalidScalarBoundary); // A run
                        // cannot split a surrogate pair even though level_at can
                        // observe the low unit's actual level record.
                    }
                }
            }
            #[cfg(feature = "paged-shaping-frontier")]
            Resolution::VisibleParagraphs(visible) => {
                let range = self.source.start + start as usize..self.source.start + end as usize;
                visible.require_range(&range)?;
                if !(range.is_empty() && range.start == self.source.end) {
                    visible.range_mut(&range)?.checked_range(range)?;
                }
                self._snapshot.original_backing_health()?;
                self.resolution.healthy()?;
            }
        }
        Ok(start..end)
    }
}

/// Map only a real surrogate-interior endpoint to the public boundary error;
/// mapped-source read/reclaim failures retain their original typed cause.
fn scalar_endpoint_error(error: MarkdownDocumentBufferError) -> BidiError {
    match error {
        MarkdownDocumentBufferError::NotUnicodeBoundary => BidiError::InvalidScalarBoundary,
        other => BidiError::Source(other),
    }
}

#[derive(Clone, Copy)]
struct Text {
    byte: u32,
    bracket: u32,
    pair_end: u32,
    pair_start_run: u32,
    pair_end_run: u32,
    original: BidiClass,
    processing: BidiClass,
    level: u8,
    flags: u8,
}

impl Text {
    fn read(store: &mut Store, id: u32) -> Result<Self, BidiError> {
        let bytes = store.read(Table::Text, id)?;
        Ok(Self {
            byte: read_u32(&bytes, 0),
            bracket: read_u32(&bytes, 4),
            pair_end: read_u32(&bytes, 8),
            pair_start_run: read_u32(&bytes, 12),
            pair_end_run: read_u32(&bytes, 16),
            original: decode_class(bytes[20])?,
            processing: decode_class(bytes[21])?,
            level: bytes[22],
            flags: bytes[23],
        })
    }

    fn write(self, store: &mut Store, id: u32) -> Result<(), BidiError> {
        let mut bytes = [0; 32];
        for (offset, value) in [
            (0, self.byte),
            (4, self.bracket),
            (8, self.pair_end),
            (12, self.pair_start_run),
            (16, self.pair_end_run),
        ] {
            write_u32(&mut bytes, offset, value);
        }
        bytes[20] = encode_class(self.original);
        bytes[21] = encode_class(self.processing);
        bytes[22] = self.level;
        bytes[23] = self.flags;
        store.write(Table::Text, id, bytes)
    }

    fn continuation(self) -> bool {
        self.flags & 2 != 0
    }
    fn scalar_len(self) -> Result<u32, BidiError> {
        if self.continuation() {
            Err(BidiError::CorruptMetadata)
        } else {
            Ok(if self.flags & 1 != 0 { 2 } else { 1 })
        }
    }
}

#[derive(Clone, Copy)]
struct Paragraph {
    start: u32,
    end: u32,
    base: u8,
}

impl Paragraph {
    fn read(store: &mut Store, id: u32) -> Result<Self, BidiError> {
        let bytes = store.read(Table::Paragraph, id)?;
        Ok(Self {
            start: read_u32(&bytes, 0),
            end: read_u32(&bytes, 4),
            base: bytes[8],
        })
    }
    fn write(self, store: &mut Store, id: u32) -> Result<(), BidiError> {
        let mut bytes = [0; 32];
        write_u32(&mut bytes, 0, self.start);
        write_u32(&mut bytes, 4, self.end);
        bytes[8] = self.base;
        store.write(Table::Paragraph, id, bytes)
    }
}

#[derive(Clone, Copy)]
struct Run {
    start: u32,
    end: u32,
    previous: u32,
    next: u32,
}

impl Run {
    fn read(store: &mut Store, id: u32) -> Result<Self, BidiError> {
        let bytes = store.read(Table::Run, id)?;
        Ok(Self {
            start: read_u32(&bytes, 0),
            end: read_u32(&bytes, 4),
            previous: read_u32(&bytes, 8),
            next: read_u32(&bytes, 12),
        })
    }
    fn write(self, store: &mut Store, id: u32) -> Result<(), BidiError> {
        let mut bytes = [0; 32];
        for (offset, value) in [
            (0, self.start),
            (4, self.end),
            (8, self.previous),
            (12, self.next),
        ] {
            write_u32(&mut bytes, offset, value);
        }
        store.write(Table::Run, id, bytes)
    }
}

#[derive(Clone, Copy)]
struct Sequence {
    first: u32,
    last: u32,
    sos: BidiClass,
    eos: BidiClass,
}

impl Sequence {
    fn read(store: &mut Store, id: u32) -> Result<Self, BidiError> {
        let bytes = store.read(Table::Sequence, id)?;
        Ok(Self {
            first: read_u32(&bytes, 0),
            last: read_u32(&bytes, 4),
            sos: decode_class(bytes[8])?,
            eos: decode_class(bytes[9])?,
        })
    }
    fn write(self, store: &mut Store, id: u32) -> Result<(), BidiError> {
        let mut bytes = [0; 32];
        write_u32(&mut bytes, 0, self.first);
        write_u32(&mut bytes, 4, self.last);
        bytes[8] = encode_class(self.sos);
        bytes[9] = encode_class(self.eos);
        store.write(Table::Sequence, id, bytes)
    }
}

fn direction(level: u8) -> BidiClass {
    if level & 1 == 0 {
        BidiClass::L
    } else {
        BidiClass::R
    }
}
fn removed(class: BidiClass) -> bool {
    use BidiClass::*;
    matches!(class, RLE | LRE | RLO | LRO | PDF | BN)
}

// This fixed metadata encoding is independent of third-party enum discriminants
// and needs no unsafe conversion between their representations.
fn encode_class(class: BidiClass) -> u8 {
    use BidiClass::*;
    match class {
        AL => 0,
        AN => 1,
        B => 2,
        BN => 3,
        CS => 4,
        EN => 5,
        ES => 6,
        ET => 7,
        FSI => 8,
        L => 9,
        LRE => 10,
        LRI => 11,
        LRO => 12,
        NSM => 13,
        ON => 14,
        PDF => 15,
        PDI => 16,
        R => 17,
        RLE => 18,
        RLI => 19,
        RLO => 20,
        S => 21,
        WS => 22,
    }
}
fn decode_class(value: u8) -> Result<BidiClass, BidiError> {
    use BidiClass::*;
    Ok(match value {
        0 => AL,
        1 => AN,
        2 => B,
        3 => BN,
        4 => CS,
        5 => EN,
        6 => ES,
        7 => ET,
        8 => FSI,
        9 => L,
        10 => LRE,
        11 => LRI,
        12 => LRO,
        13 => NSM,
        14 => ON,
        15 => PDF,
        16 => PDI,
        17 => R,
        18 => RLE,
        19 => RLI,
        20 => RLO,
        21 => S,
        22 => WS,
        _ => return Err(BidiError::CorruptMetadata),
    })
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod unicode_data_tests;
