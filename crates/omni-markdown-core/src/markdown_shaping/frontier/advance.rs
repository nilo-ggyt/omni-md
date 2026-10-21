//! Incremental lower bounds for all default horizontal advances of the same actual font. This
//! proof alone grants neither soft-line selection nor source-slice authority.

use harfrust::{GlyphId, ShapePlan, ShapePlanKey};
use std::sync::{Arc, Weak};

use super::{
    MarkdownLayoutFont, MarkdownLayoutFontAuthority, MarkdownLayoutTextAuthority,
    MarkdownLayoutTextSource, MarkdownShapingCancellation, MarkdownShapingError, ValidatedOptions,
};
use crate::markdown_layout_font::frontier::FontFrontierPlan;
use crate::{MarkdownDocumentBufferError, MarkdownOriginalBackingObserver};

/// Query at most 128 actual glyphs per step; full-font closure work and body-callback work are
/// accounted separately, without treating either budget as wall time or RSS.
const ADVANCE_SCAN_STEP: usize = 128;

#[derive(Debug, thiserror::Error)]
pub(crate) enum AdvanceScanError {
    #[error("原advance扫描的Source、Font或取消状态失效：{0}")]
    Shaping(#[from] MarkdownShapingError),
    #[error("原实际计划尚未证明无跨输入位置调整")]
    UnsupportedPlan,
    #[error("原实际计划不属于这组Font与完整整形选项")]
    PlanMismatch,
    #[error("原advance扫描的实际glyph元数据或单步配额无效")]
    InvalidRequest,
    #[error("原advance扫描尚未完成或已经撤销")]
    Incomplete,
}
type Result<T> = std::result::Result<T, AdvanceScanError>;

/// Borrow the same original Source/font for construction; names and copied public ranges cannot
/// replace these identities.
pub(super) struct AdvanceScanOrigin<'a> {
    pub source: &'a MarkdownLayoutTextSource,
    pub source_authority: &'a MarkdownLayoutTextAuthority,
    pub font: &'a MarkdownLayoutFont,
    pub font_authority: &'a MarkdownLayoutFontAuthority,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AdvanceScanAnswer {
    Pending,
    Complete,
    /// One actual negative value rejects a nonnegative-suffix proof; never clamp it to zero or
    /// ignore the remaining glyph domain to manufacture success.
    Negative {
        glyph: u32,
        advance_26_6: i32,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct AdvanceScanStep {
    pub answer: AdvanceScanAnswer,
    pub queried: usize,
    pub total_queried: u32,
}

/// Retain fixed metadata and the original font, without source bytes, a glyph list, or a body index.
pub(super) struct DefaultAdvanceScan {
    font: Option<MarkdownLayoutFont>,
    font_authority: MarkdownLayoutFontAuthority,
    source_authority: MarkdownLayoutTextAuthority,
    source_backing: Option<MarkdownOriginalBackingObserver>,
    plan: Arc<ShapePlan>,
    options: ValidatedOptions,
    cancellation: MarkdownShapingCancellation,
    count: u32,
    next: u32,
    minimum: i32,
    answer: AdvanceScanAnswer,
}

/// Issued only after all actual glyphs are scanned; weak tickets extend no font/plan/Source lifetime.
/// The original frontier still independently certifies unread text's ASCII domain, stable seams,
/// and actual grapheme boundaries.
pub(super) struct NonnegativeDefaultAdvanceProof {
    font: MarkdownLayoutFontAuthority,
    source: MarkdownLayoutTextAuthority,
    source_backing: Option<MarkdownOriginalBackingObserver>,
    plan: Weak<ShapePlan>,
    options: ValidatedOptions,
    glyphs: u32,
    minimum_26_6: i32,
    cancellation: MarkdownShapingCancellation,
}

fn same_options(left: &ValidatedOptions, right: &ValidatedOptions) -> bool {
    left.direction == right.direction
        && left.script == right.script
        && left.language == right.language
        && left.points.to_bits() == right.points.to_bits()
        && left.scale == right.scale
        && left.output_divisor == right.output_divisor
        && left.beginning_of_text == right.beginning_of_text
        && left.end_of_text == right.end_of_text
        && left.features == right.features
}

impl DefaultAdvanceScan {
    /// Require the actual plan's narrow ASCII dependency certificate before querying every glyph;
    /// names, a current prefix, or samples cannot establish future lower bounds.
    pub(super) fn new(
        origin: AdvanceScanOrigin<'_>,
        options: &ValidatedOptions,
        plan_owner: &FontFrontierPlan,
        cancellation: MarkdownShapingCancellation,
    ) -> Result<Self> {
        origin.source.health().map_err(MarkdownShapingError::from)?;
        if !origin.source.permits(origin.source_authority) {
            return Err(MarkdownShapingError::SourceMismatch.into());
        }
        if !origin.font.permits(origin.font_authority) {
            return Err(MarkdownShapingError::FontMismatch.into());
        }
        if cancellation.requested() {
            return Err(MarkdownShapingError::Cancelled.into());
        } // Check the original Source failure first; a real EIO cannot become a clean cancellation.
        if !plan_owner.permits_font(origin.font) {
            return Err(AdvanceScanError::PlanMismatch);
        }
        let plan = plan_owner.plan();
        let key = ShapePlanKey::new(origin.font.font(), Some(options.script), options.direction)
            .language(options.language.as_ref())
            .features(&options.features);
        if !key.matches(&plan) {
            return Err(AdvanceScanError::PlanMismatch);
        }
        plan.ascii_frontier_capability(origin.font.font())
            .map_err(|_| AdvanceScanError::UnsupportedPlan)?;
        let count = origin.font.metadata().glyph_count;
        if count == 0 || count > u32::from(u16::MAX) {
            return Err(AdvanceScanError::InvalidRequest);
        } // Glyph IDs outside actual maxp cannot contribute invented default/missing values to this proof.
        origin.source.health().map_err(MarkdownShapingError::from)?;
        if cancellation.requested() {
            return Err(MarkdownShapingError::Cancelled.into());
        }
        Ok(Self {
            font: Some(origin.font.clone()),
            font_authority: origin.font_authority.clone(),
            source_authority: origin.source_authority.clone(),
            source_backing: origin.source.original_backing_observer(),
            plan,
            options: options.clone(),
            cancellation,
            count,
            next: 0,
            minimum: i32::MAX,
            answer: AdvanceScanAnswer::Pending,
        })
    }

    fn guard(
        &self,
        source: &MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
    ) -> Result<()> {
        if let Some(errno) = self
            .source_backing
            .as_ref()
            .and_then(|value| value.first_errno())
        {
            return Err(MarkdownShapingError::from(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(errno),
            )
            .into());
        } // A healthy foreign capture or later cancellation cannot conceal original Source failure.
        source.health().map_err(MarkdownShapingError::from)?;
        if authority != &self.source_authority || !source.permits(authority) {
            return Err(MarkdownShapingError::SourceMismatch.into());
        }
        let font = self.font.as_ref().ok_or(AdvanceScanError::Incomplete)?;
        if !font.permits(&self.font_authority) {
            return Err(MarkdownShapingError::FontMismatch.into());
        }
        if self.cancellation.requested() {
            return Err(MarkdownShapingError::Cancelled.into());
        }
        Ok(())
    }

    /// Perform a fixed number of actual default-metric queries per call; the caller accounts this
    /// work in the original worker schedule.
    pub(super) fn step(
        &mut self,
        source: &MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        budget: usize,
    ) -> Result<AdvanceScanStep> {
        let result = (|| {
            self.guard(source, authority)?;
            if budget == 0 {
                return Err(AdvanceScanError::InvalidRequest);
            } // A zero budget performs no progress and cannot issue a complete-font proof.
            let end = self
                .next
                .saturating_add(budget.min(ADVANCE_SCAN_STEP) as u32)
                .min(self.count);
            let start = self.next;
            if self.answer == AdvanceScanAnswer::Pending {
                let font = self.font.as_ref().ok_or(AdvanceScanError::Incomplete)?;
                let shaper = font
                    .shaper_at_scale(self.options.scale)
                    .map_err(|_| AdvanceScanError::InvalidRequest)?;
                while self.next < end {
                    // Cover glyphs not yet encountered in text too. Use the same immutable platform
                    // table as actual shaping when present; otherwise read actual hmtx. Proving
                    // file metrics while executing different advances would not certify the output.
                    let raw = shaper.glyph_h_advance(GlyphId::new(self.next));
                    let advance = self.options.quantize(raw);
                    let glyph = self.next;
                    self.next += 1;
                    self.minimum = self.minimum.min(advance);
                    if advance < 0 {
                        self.answer = AdvanceScanAnswer::Negative {
                            glyph,
                            advance_26_6: advance,
                        };
                        break;
                    } // The first actual negative rejects certification; later positives cannot prove arbitrary suffixes nonnegative.
                }
                if self.answer == AdvanceScanAnswer::Pending && self.next == self.count {
                    self.answer = AdvanceScanAnswer::Complete;
                }
            }
            self.guard(source, authority)?; // Original Source failure after font access still precedes completion/cancellation.
            Ok(AdvanceScanStep {
                answer: self.answer,
                queried: (self.next - start) as usize,
                total_queried: self.next,
            })
        })();
        if result.is_err() {
            drop(self.font.take()); // Failure permanently returns this scan's font; another Source/options cannot resume it.
        }
        result
    }

    /// Consume a complete scan. Negative/incomplete states produce no proof and retain no scan
    /// owner that could later resume after failure.
    pub(super) fn finish(
        self,
        source: &MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
    ) -> Result<NonnegativeDefaultAdvanceProof> {
        self.guard(source, authority)?;
        if self.answer != AdvanceScanAnswer::Complete || self.next != self.count {
            return Err(AdvanceScanError::Incomplete);
        }
        Ok(NonnegativeDefaultAdvanceProof {
            font: self.font_authority.clone(),
            source: self.source_authority.clone(),
            source_backing: self.source_backing.clone(),
            plan: Arc::downgrade(&self.plan),
            options: self.options.clone(),
            glyphs: self.count,
            minimum_26_6: self.minimum,
            cancellation: self.cancellation.clone(),
        })
    }
}

impl NonnegativeDefaultAdvanceProof {
    /// Check the original font/plan/full options only; this boolean grants no source slice,
    /// emergency-line selection, or Ready authority.
    pub(super) fn matches(
        &self,
        origin: AdvanceScanOrigin<'_>,
        options: &ValidatedOptions,
        plan_owner: &FontFrontierPlan,
    ) -> Result<bool> {
        if let Some(errno) = self
            .source_backing
            .as_ref()
            .and_then(|value| value.first_errno())
        {
            return Err(MarkdownShapingError::from(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(errno),
            )
            .into());
        }
        origin.source.health().map_err(MarkdownShapingError::from)?;
        if self.cancellation.requested() {
            return Err(MarkdownShapingError::Cancelled.into());
        } // Completed lookup does not waive original cancellation or let another same-font operation revive this proof.
        let plan = plan_owner.plan();
        Ok(self.source == *origin.source_authority
            && origin.source.permits(origin.source_authority)
            && origin.font.permits(&self.font)
            && origin.font.permits(origin.font_authority)
            && plan_owner.permits_font(origin.font)
            && self.plan.ptr_eq(&Arc::downgrade(&plan))
            && self.plan.strong_count() != 0
            && same_options(&self.options, options)
            && self.glyphs == origin.font.metadata().glyph_count
            && self.minimum_26_6 >= 0)
    }

    pub(super) fn minimum_26_6(&self) -> i32 {
        self.minimum_26_6
    }
}
