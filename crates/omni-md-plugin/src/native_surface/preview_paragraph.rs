//! Shared scalar geometry for paged preview. Width, line height, and painting
//! consume this same contract without owning text, fonts, or a layout graph.

use super::paged_font_geometry::NativeMarkdownPagedFontMetricValues;
use super::paged_line_fonts::NativeMarkdownPreviewFontMetrics;
use omni_markdown_core::MarkdownPresentationStyles as Styles;
use omni_markdown_core::{
    MarkdownComposedLineFacts, MarkdownComposedLinePartFacts, MarkdownComposedLinePartKind,
    MarkdownPresentationMode,
};

/// Producer steps and bounded viewport preparation share this font/paragraph
/// accumulator; neither creates a separate document-wide line-height catalog.
pub(in crate::native_surface) struct PreviewLineMetrics {
    pub ascent: f64,
    pub descent: f64,
    pub leading: f64,
    pub paragraph: PreviewParagraph,
    mode: MarkdownPresentationMode,
    system_above: f64,
    system_below: f64,
    control_above: f64,
    control_below: f64,
    has_text: bool,
}

impl PreviewLineMetrics {
    /// Mode comes from the original Body/Scene geometry identity. An empty body
    /// style cannot select a different mode's system line slot.
    pub fn new(mode: MarkdownPresentationMode) -> Self {
        Self {
            ascent: 0.0,
            descent: 0.0,
            leading: f64::NEG_INFINITY,
            paragraph: PreviewParagraph::LITERAL,
            mode,
            system_above: 0.0,
            system_below: 0.0,
            control_above: 0.0,
            control_below: 0.0,
            has_text: false,
        }
    }

    /// Accumulate actual sealed part metrics. Glyphs/tabs establish text slots;
    /// default newline controls cannot borrow adjacent text to invent styling.
    pub fn add(
        &mut self,
        part: &MarkdownComposedLinePartFacts,
        metrics: NativeMarkdownPagedFontMetricValues,
        preview: Option<NativeMarkdownPreviewFontMetrics>,
    ) -> Option<()> {
        self.ascent = self.ascent.max(metrics.ascent);
        self.descent = self.descent.max(metrics.descent);
        self.leading = self.leading.max(metrics.leading);
        if self.mode == MarkdownPresentationMode::InlinePreview {
            let preview =
                preview.expect("Preview实际Font持系统行槽；不能用Source浮点或猜测取整替换");
            let offset =
                super::text_attributes::native_markdown_baseline_offset(part.presentation, false)
                    .unwrap_or(0.0);
            // The measured TextKit contract raises the baseline for superscript
            // and extends the lower edge for subscript. EOL controls still do
            // not increase the height of a line containing actual text.
            let above = preview.baseline + offset.max(0.0);
            let below = (preview.line_height - preview.baseline - offset).max(0.0);
            if matches!(
                part.kind,
                MarkdownComposedLinePartKind::Glyphs | MarkdownComposedLinePartKind::Tab
            ) {
                self.has_text = true;
                self.system_above = self.system_above.max(above);
                self.system_below = self.system_below.max(below);
            } else {
                self.control_above = self.control_above.max(above);
                self.control_below = self.control_below.max(below);
            } // Only genuinely empty/EOL lines use their own control-font slot.
        } // Mixed fonts retain separate measured bounds above and below the baseline.
        if part.kind != MarkdownComposedLinePartKind::FoldedSection {
            let paragraph = PreviewParagraph::from_styles(part.presentation)
                .unwrap_or(PreviewParagraph::LITERAL);
            if paragraph.inset_26_6() != part.paragraph_inset_26_6 {
                return None; // Sealed wrap width must match the final frame; a paint offset cannot repair old output.
            }
            if paragraph != PreviewParagraph::LITERAL {
                if self.paragraph != PreviewParagraph::LITERAL && self.paragraph != paragraph {
                    return None; // Conflicting paragraph roles cannot share one source line; retain failure rather than choosing one.
                }
                self.paragraph = paragraph;
            }
        }
        Some(())
    }

    /// Apply outer spacing only at the same sealed source paragraph's actual
    /// endpoints. A viewport edge is not a paragraph boundary.
    pub fn geometry(&self, line: &MarkdownComposedLineFacts) -> Option<PreviewLineGeometry> {
        if self.mode == MarkdownPresentationMode::InlinePreview {
            let (above, below) = if self.has_text {
                (self.system_above, self.system_below)
            } else {
                (self.control_above, self.control_below)
            };
            return self.paragraph.measure_preview(
                above,
                below,
                line.source_utf16.start == line.paragraph_utf16.start
                    && line.paragraph_utf16.start != 0,
                line.source_utf16.end == line.paragraph_utf16.end,
            ); // No preceding paragraph exists at document start; paragraphSpacingBefore adds no extra initial gap.
        }
        self.paragraph.measure(
            self.ascent,
            self.descent,
            self.leading,
            line.source_utf16.start == line.paragraph_utf16.start,
            line.source_utf16.end == line.paragraph_utf16.end,
        )
    }
}

/// A background paints an already measured frame; it owns no height, inset, or
/// source-coordinate conversion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::native_surface) enum PreviewParagraphBackground {
    Code,
    Quote,
    Rule,
}

/// Finite values from the original AppKit paragraph contract. Actual table
/// layout remains the sole authority for table-row heights.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::native_surface) struct PreviewParagraph {
    pub before: f64,
    pub after: f64,
    pub minimum_line_height: f64,
    pub maximum_line_height: f64,
    pub line_height_multiple: f64,
    pub inset: f64,
    pub background: Option<PreviewParagraphBackground>,
}

/// One vertical allocation from actual font metrics, shared by line frames,
/// baselines, and scroll height rather than independently estimated for each.
#[derive(Clone, Copy, Debug)]
pub(in crate::native_surface) struct PreviewLineGeometry {
    pub advance: f64,
    pub baseline_from_top: f64,
    pub text_height: f64,
    pub before: f64,
    pub after: f64,
}

#[cfg(all(
    feature = "native-paged-body-probe",
    feature = "native-markdown-runtime-probe"
))]
/// Plain facts from actual bounded-window lines. Only the Body/Find probe pair
/// consumes these observations; they retain no Source, Reader, font, or Native claim.
pub(in crate::native_surface) struct PreviewLineObservation {
    pub source: std::ops::Range<usize>,
    pub paragraph: std::ops::Range<usize>,
    pub frame: objc2_foundation::NSRect,
    pub baseline: objc2_foundation::NSPoint,
    pub text_height: f64,
    pub before: f64,
    pub after: f64,
}

impl PreviewParagraph {
    pub const LITERAL: Self = Self {
        before: 0.0,
        after: 0.0,
        minimum_line_height: 0.0,
        maximum_line_height: 0.0,
        line_height_multiple: 0.0,
        inset: 0.0,
        background: None,
    };

    /// Interpret already published semantic styles. This function grants no
    /// marker-hiding authority and never treats an unknown container as ready body text.
    pub fn from_styles(styles: Styles) -> Option<Self> {
        let hidden = styles.contains(Styles::HIDDEN_MARKER);
        if hidden && styles.contains(Styles::FENCED_CODE) {
            return None; // Hidden fence controls receive no code padding, background, or minimum line height.
        }
        if styles.contains(Styles::TABLE_ROW_PLACEHOLDER)
            || (hidden && styles.contains(Styles::TABLE_DELIMITER))
        {
            return None; // Table-owned slots and delimiters cannot receive body paragraph geometry again.
        }

        let mut paragraph = Self::LITERAL;
        let heading = [
            Styles::HEADING_1,
            Styles::HEADING_2,
            Styles::HEADING_3,
            Styles::HEADING_4,
            Styles::HEADING_5,
            Styles::HEADING_6,
        ]
        .into_iter()
        .any(|role| styles.contains(role));
        if heading {
            paragraph.before = 8.0;
            paragraph.after = 6.0;
            paragraph.line_height_multiple = 1.08;
        } // Inline emphasis and links change glyphs while the original heading owns paragraph spacing.

        if hidden && styles.contains(Styles::HORIZONTAL_RULE) {
            paragraph.minimum_line_height = 12.0;
            paragraph.maximum_line_height = 12.0;
            paragraph.before = 6.0;
            paragraph.after = 6.0;
            paragraph.background = Some(PreviewParagraphBackground::Rule);
            return Some(paragraph); // Active rules remain literal; only original hidden semantics authorize the thin rule slot.
        }
        if styles.contains(Styles::FENCED_CODE) {
            paragraph.minimum_line_height = 22.0;
            paragraph.after = 0.0;
            paragraph.inset = 6.0;
            paragraph.before += paragraph.inset;
            paragraph.after += paragraph.inset;
            paragraph.background = Some(PreviewParagraphBackground::Code);
            return Some(paragraph); // Insets first reduce available width; actual first/last lines receive outer padding.
        }
        if styles.contains(Styles::BLOCK_QUOTE) {
            paragraph.line_height_multiple = 1.2;
            paragraph.after = 2.0;
            paragraph.inset = 7.0;
            paragraph.before += paragraph.inset;
            paragraph.after += paragraph.inset;
            paragraph.background = Some(PreviewParagraphBackground::Quote);
            return Some(paragraph); // Preserve 6-point padding plus the 1-point border, under the original parser's container authority.
        }
        heading.then_some(paragraph)
    }

    /// Existing insets are exact integer points. Seal the same value in the
    /// Item's 26.6 field without floating-point inference of available width.
    pub fn inset_26_6(self) -> i32 {
        (self.inset * 64.0) as i32
    }

    /// Scale the system's natural slot by the original paragraph multiple, then
    /// apply its min/max bounds. Extra height precedes text according to the
    /// independently measured TextKit baseline contract.
    fn measure_preview(
        self,
        above: f64,
        below: f64,
        first: bool,
        last: bool,
    ) -> Option<PreviewLineGeometry> {
        let natural = above + below;
        let mut text_height = if self.line_height_multiple == 0.0 {
            natural
        } else {
            natural * self.line_height_multiple
        };
        if self.minimum_line_height != 0.0 {
            text_height = text_height.max(self.minimum_line_height);
        }
        if self.maximum_line_height != 0.0 {
            text_height = text_height.min(self.maximum_line_height);
        }
        let before = if first { self.before } else { 0.0 };
        let after = if last { self.after } else { 0.0 };
        let baseline_from_top = before + above + text_height - natural;
        let advance = before + text_height + after;
        if !natural.is_finite()
            || natural <= 0.0
            || !advance.is_finite()
            || advance <= 0.0
            || !baseline_from_top.is_finite()
            || !text_height.is_finite()
            || text_height <= 0.0
        {
            return None; // Minimum height or clamping cannot certify an invalid system-font or paragraph result.
        }
        Some(PreviewLineGeometry {
            advance,
            baseline_from_top,
            text_height,
            before,
            after,
        })
    }

    /// Use the sealed part's actual registered font. Only the source paragraph's
    /// genuine first and last soft lines receive outer spacing.
    pub fn measure(
        self,
        ascent: f64,
        descent: f64,
        leading: f64,
        first: bool,
        last: bool,
    ) -> Option<PreviewLineGeometry> {
        let natural = ascent + descent + leading;
        if !natural.is_finite() || natural <= 0.0 {
            return None; // Paragraph minimum height cannot turn invalid font metrics into apparently valid layout.
        }
        let mut text_height = if self.line_height_multiple == 0.0 {
            natural
        } else {
            natural * self.line_height_multiple
        };
        if self.minimum_line_height != 0.0 {
            text_height = text_height.max(self.minimum_line_height);
        }
        if self.maximum_line_height != 0.0 {
            text_height = text_height.min(self.maximum_line_height);
        }
        let before = if first { self.before } else { 0.0 };
        let after = if last { self.after } else { 0.0 };
        // Preserve literal mode's original arithmetic and baseline order, so
        // shared preview geometry does not change existing Source float results.
        let extra = if text_height == natural {
            leading
        } else {
            text_height - ascent - descent
        };
        let baseline_from_top = before + ascent + extra / 2.0;
        let advance = before + text_height + after;
        if !advance.is_finite()
            || advance <= 0.0
            || !baseline_from_top.is_finite()
            || !text_height.is_finite()
            || text_height <= 0.0
        {
            return None; // Return actual geometry failure without clipping height or painting a partial candidate.
        }
        Some(PreviewLineGeometry {
            advance,
            baseline_from_top,
            text_height,
            before,
            after,
        })
    }
}
