//! Alignment extent for one actual LineEnd; trailing whitespace leaves alignment width, not glyph/caret/selection domains.

use super::*;
use omni_markdown_core::{
    MarkdownComposedLineTrailingWhitespace, MarkdownComposedLineTrailingWhitespaceStep,
    MarkdownLineComposeBudget, MarkdownShapingDirection, MarkdownTableAlignment,
};

/// Carry the original weak line handle/completed actual visible extent; callers cannot manufacture alignment from arbitrary floats.
pub struct NativeMarkdownMeasuredLine {
    handle: MarkdownComposedLine,
    extent: Option<Extent>,
}

pub(super) struct Extent {
    left: f64,
    right: f64,
}

impl From<MarkdownComposedLine> for NativeMarkdownMeasuredLine {
    fn from(handle: MarkdownComposedLine) -> Self {
        Self {
            handle,
            extent: None,
        } // Literal/Left needs no tail measurement; Center/Right requires an actual completed extent at consumption.
    }
}

impl NativeMarkdownMeasuredLine {
    pub fn facts(&self) -> &omni_markdown_core::MarkdownComposedLineFacts {
        self.handle.facts()
    }

    pub(in crate::native_surface) fn handle(&self) -> &MarkdownComposedLine {
        &self.handle
    }

    pub(super) fn into_parts(self) -> (MarkdownComposedLine, Option<Extent>) {
        (self.handle, self.extent)
    }
}

pub(super) fn alignment_origin_x(
    alignment: MarkdownTableAlignment,
    width: f64,
    inset: f64,
    extent: Option<&Extent>,
) -> Result<f64> {
    // Available width belongs to this actual layout lane, not the horizontal clip or full ScrollDocument.
    // Alignment translates the sealed line only; all trailing whitespace/original selection follows that same baseline.
    let available = width - inset * 2.0;
    if !width.is_finite() || !inset.is_finite() || available <= 0.0 || width >= MAX_POINT {
        return Err(NativeMarkdownComposedViewportError::InvalidGeometry);
    }
    if alignment == MarkdownTableAlignment::Left {
        return Ok(inset); // Left alignment consumes no trailing-whitespace measurement and changes neither soft-line advances nor selection endpoints.
    }
    let extent = extent.ok_or(NativeMarkdownComposedViewportError::Pending)?;
    // Logical RTL trailing whitespace may lie at visual left. Subtract actual left in addition to using visible width,
    // otherwise paint may look centered while Cell caret/Find/hits retain the whole trailing-space offset.
    let remaining = available - (extent.right - extent.left);
    let offset = match alignment {
        MarkdownTableAlignment::Left => unreachable!("左对齐已经沿原入口返回"),
        MarkdownTableAlignment::Center => remaining / 2.0,
        MarkdownTableAlignment::Right => remaining,
    };
    let origin = inset + offset - extent.left;
    if !origin.is_finite() || origin.abs() >= MAX_POINT {
        return Err(NativeMarkdownComposedViewportError::InvalidGeometry);
    }
    Ok(origin)
}

/// The original LineMetrics owns this tail query, retaining no reader/font/Source strong claims or second line directory.
pub(in crate::native_surface) struct NativeMarkdownLineAlignmentMeasure {
    query: MarkdownComposedLineTrailingWhitespace,
    range: Range<usize>,
    ordinal: usize,
    parts: usize,
    observed: usize,
    trim_end: Option<usize>,
    extent: Option<Extent>,
}

impl NativeMarkdownLineAlignmentMeasure {
    pub(in crate::native_surface) fn new(
        output: &RefCell<MarkdownComposedLines>,
        line: &MarkdownComposedLine,
    ) -> Result<Self> {
        // begin captures weak original identity only; actual Source/directory reads occur in later budgeted steps.
        // Do not reuse this measurement across size changes; publish completion with the current bounded visible handle only.
        let query = output
            .try_borrow()
            .map_err(|_| NativeMarkdownComposedViewportError::Busy)?
            .begin_line_trailing_whitespace(line)?;
        Ok(Self {
            query,
            range: line.facts().source_utf16.clone(),
            ordinal: line.facts().ordinal,
            parts: line.facts().parts,
            observed: 0,
            trim_end: None,
            extent: None,
        })
    }

    /// Use the caller's remaining Source/GCB quantum and retain only actual UTF-16 end; glyph IDs cannot identify whitespace.
    pub(in crate::native_surface) fn step_source(
        &mut self,
        output: &RefCell<MarkdownComposedLines>,
        line: &MarkdownComposedLine,
        budget: MarkdownLineComposeBudget,
    ) -> Result<MarkdownComposedLineTrailingWhitespaceStep> {
        let step = output
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownComposedViewportError::Busy)?
            .step_line_trailing_whitespace(line, &mut self.query, budget)?;
        self.trim_end = step.answer;
        Ok(step)
    }

    pub(in crate::native_surface) fn source_ready(&self) -> bool {
        self.trim_end.is_some()
    }

    /// Offscreen Find uses the same completed extent, including actual left for RTL trailing whitespace rather than subtracting width alone.
    pub(in crate::native_surface) fn origin_x(
        &self,
        alignment: MarkdownTableAlignment,
        width: f64,
        inset: f64,
    ) -> Result<f64> {
        // Find also waits for complete line measurement; the target part's local advance alone cannot center the line.
        // A completed all-whitespace line has actual empty visible extent, not a missing font/unknown width.
        if self.observed != self.parts || self.trim_end.is_none() {
            return Err(NativeMarkdownComposedViewportError::Pending);
        }
        if self.trim_end == Some(self.range.start) {
            return alignment_origin_x(
                alignment,
                width,
                inset,
                Some(&Extent {
                    left: 0.0,
                    right: 0.0,
                }),
            );
        }
        alignment_origin_x(alignment, width, inset, self.extent.as_ref())
    }

    /// Reuse original font/sealed-part caret without relayout or trimming source text; advance parts under the existing metadata budget.
    pub(in crate::native_surface) fn observe_part(
        &mut self,
        output: &RefCell<MarkdownComposedLines>,
        part: &MarkdownComposedLinePart,
        font: &NativeMarkdownLineFont,
        current: &dyn Fn() -> bool,
    ) -> Result<()> {
        let trim_end = self
            .trim_end
            .ok_or(NativeMarkdownComposedViewportError::Pending)?;
        let facts = part.facts();
        if self.observed == self.parts
            || facts.source_utf16.start < self.range.start
            || facts.source_utf16.end > self.range.end
        {
            return Err(NativeMarkdownComposedViewportError::InvalidCoverage);
        }
        if facts.source_utf16.start < trim_end {
            // Actual sealed facts supply a full part's visual advance without reading whole-line text or glyph arrays,
            // or inferring whitespace from glyph ID, nominal space width, or UTF-16 length.
            let mut left = facts.origin_x_26_6 as f64 / 64.0;
            let mut right = left + facts.x_advance_26_6 as f64 / 64.0;
            if trim_end < facts.source_utf16.end {
                // Core promoted the endpoint to a complete original grapheme, retaining mixed Prepend/whitespace clusters.
                // Only pure trailing whitespace leaves the extent; existing caret GDEF/cluster rules remain the sole geometry source.
                let caret = super::super::paged_line_geometry::composed_part_caret(
                    output,
                    part,
                    font,
                    trim_end,
                    CGPoint::ZERO,
                    current,
                );
                // After font calls check actual Source/reader failure first, preventing EIO from becoming foreground revocation.
                output
                    .try_borrow()
                    .map_err(|_| NativeMarkdownComposedViewportError::Busy)?
                    .require(part.source_authority())?;
                let caret = caret?;
                match facts.direction {
                    MarkdownShapingDirection::LeftToRight => right = caret.baseline.x,
                    MarkdownShapingDirection::RightToLeft => left = caret.baseline.x,
                    _ => return Err(NativeMarkdownComposedViewportError::InvalidGeometry),
                }
            }
            if !left.is_finite() || !right.is_finite() || right < left {
                return Err(NativeMarkdownComposedViewportError::InvalidGeometry);
            }
            if let Some(extent) = self.extent.as_mut() {
                // Visual parts may have mixed directions; union actual left/right edges rather than sum logical advances
                // from an assumed zero origin or substitute the final logical part for complete line extent.
                extent.left = extent.left.min(left);
                extent.right = extent.right.max(right);
            } else {
                self.extent = Some(Extent { left, right });
            }
        }
        // Visit/account trailing parts even though they contribute no alignment extent; keep their complete handles/glyphs
        // so hits, selection, and IME can reach those spaces without a second cropped line.
        self.observed += 1;
        Ok(())
    }

    /// Only a complete original line produces MeasuredLine; all-whitespace lines use actual empty extent while retaining trailing glyphs.
    pub(in crate::native_surface) fn finish(
        self,
        handle: MarkdownComposedLine,
    ) -> Result<NativeMarkdownMeasuredLine> {
        // The sole producer pairs original handle/completion; another line or arbitrary floats cannot manufacture alignment authority.
        // Closing output naturally revokes its weak handle; MeasuredLine retains no Source, FD, font, or reader.
        if self.trim_end.is_none()
            || self.observed != self.parts
            || handle.facts().source_utf16 != self.range
            || handle.facts().ordinal != self.ordinal
            || handle.facts().parts != self.parts
        {
            return Err(NativeMarkdownComposedViewportError::Pending);
        }
        let extent = match self.extent {
            Some(extent) => extent,
            None if self.trim_end == Some(self.range.start) => Extent {
                left: 0.0,
                right: 0.0,
            },
            None => return Err(NativeMarkdownComposedViewportError::InvalidCoverage),
        };
        Ok(NativeMarkdownMeasuredLine {
            handle,
            extent: Some(extent),
        })
    }
}
