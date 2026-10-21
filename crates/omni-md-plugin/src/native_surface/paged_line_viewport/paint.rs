//! Paint actual parts from one window in batches of at most 256 glyphs; synchronous budget overflow paints no prefix and failure discards the candidate.

use super::*;
use crate::native_surface::paged_glyph_paint::{
    NATIVE_MARKDOWN_PAGED_GLYPH_BATCH, NativeMarkdownPagedGlyphOrigin,
    NativeMarkdownPagedGlyphPaintError, NativeMarkdownPagedGlyphPaintRequest,
    paint_composed_glyphs,
};
use crate::native_surface::text_attributes::{
    NATIVE_MARKDOWN_CODE_BACKGROUND, NATIVE_MARKDOWN_LINK_COLOR, NATIVE_MARKDOWN_MARKER_COLOR,
    NATIVE_MARKDOWN_QUOTE_COLOR,
};
use objc2_core_graphics::CGContext;
use omni_markdown_core::{MarkdownComposedLinePartKind, MarkdownPresentationStyles};

mod grid;
pub(in crate::native_surface) use grid::{
    NativeMarkdownComposedViewportDecorationCursor, NativeMarkdownComposedViewportDecorationStep,
};
mod session;
pub use session::{
    NativeMarkdownComposedViewportPaintBudget, NativeMarkdownComposedViewportPaintCompletion,
    NativeMarkdownComposedViewportPaintOutcome, NativeMarkdownComposedViewportPaintSession,
    NativeMarkdownComposedViewportPaintStep, NativeMarkdownComposedViewportPaintWork,
};

#[derive(Debug, thiserror::Error)]
pub enum NativeMarkdownComposedViewportPaintCause {
    #[error("原窗口几何或权限失效：{0}")]
    Window(#[from] NativeMarkdownComposedViewportError),
    #[error("原part字形绘制失败：{0}")]
    Glyph(#[from] NativeMarkdownPagedGlyphPaintError),
    #[error("原part实际advance与封存事实不一致")]
    InconsistentAdvance,
    #[error("单次绘制配额超出256个字形或32条元数据")]
    InvalidBudget,
    #[error("分步绘制的原私有位图上下文或坐标已经改变")]
    ContextChanged,
    #[error("原生分步绘制只能在主线程执行")]
    WrongThread,
    #[error("实际绘制计数溢出")]
    CounterExhausted,
}

#[derive(Debug, thiserror::Error)]
#[error("{cause}；原生绘制已返回={native_draw_returned}")]
pub struct NativeMarkdownComposedViewportPaintError {
    pub cause: NativeMarkdownComposedViewportPaintCause,
    pub native_draw_returned: bool,
}
#[derive(Clone, Copy, Debug)]
pub struct NativeMarkdownComposedViewportPaintReceipt {
    pub glyphs: usize,
    pub batches: usize,
    pub missing_glyphs: usize,
}

struct Restore<'a>(&'a CGContext);
impl Drop for Restore<'_> {
    fn drop(&mut self) {
        CGContext::restore_g_state(Some(self.0));
    }
}

impl NativeMarkdownComposedViewport {
    /// Colored ink/backgrounds share the window's sRGB input space; do not infer source color from target bitmap space.
    /// Calls occur inside paint save/restore; ordinary unstyled text retains the caller's fill.
    fn fill_color(&self, context: &CGContext, color: [f64; 4]) {
        CGContext::set_fill_color_space(Some(context), Some(&self.paint_space));
        // SAFETY: Named sRGB has three RGB components plus stack alpha; the API retains no pointer after return.
        unsafe { CGContext::set_fill_color(Some(context), color.as_ptr()) };
    }

    /// Change this part's ink state only, retaining glyphs/advances and the existing Native link-color policy.
    fn ink_style(&self, part: &Part, context: &CGContext) {
        let styles = part.handle.facts().presentation;
        if styles.contains(MarkdownPresentationStyles::HIDDEN_MARKER) {
            CGContext::set_alpha(Some(context), 0.0);
        } else if styles.contains(MarkdownPresentationStyles::MARKER) {
            let [red, green, blue, alpha] = NATIVE_MARKDOWN_MARKER_COLOR;
            self.fill_color(context, [red, green, blue, alpha]);
        } else if styles.contains(MarkdownPresentationStyles::LINK) {
            let [red, green, blue, alpha] = NATIVE_MARKDOWN_LINK_COLOR;
            self.fill_color(context, [red, green, blue, alpha]);
        } else if styles.contains(MarkdownPresentationStyles::BLOCK_QUOTE) {
            // Quoted emphasis/monospace code retains actual fonts; only ink brightness follows the original quote-color contract.
            // Visible markers/links already selected their colors above; container styles cannot override them.
            let [red, green, blue, alpha] = NATIVE_MARKDOWN_QUOTE_COLOR;
            self.fill_color(context, [red, green, blue, alpha]);
        } // Alpha also affects color glyphs; exposed syntax cannot borrow LINK styling as clickable content or replace the actual font.
    }

    /// Paint sealed-window code backgrounds before Scene selection using bounded original parts, without Source reads or glyph copies.
    pub fn paint_code_backgrounds(
        &self,
        context: &CGContext,
        current: &dyn Fn() -> bool,
    ) -> std::result::Result<(), NativeMarkdownComposedViewportPaintError> {
        let mut native_draw_returned = false;
        let result = (|| -> std::result::Result<_, NativeMarkdownComposedViewportPaintCause> {
            self.require(current)?;
            CGContext::save_g_state(Some(context));
            let restore = Restore(context);
            CGContext::clip_to_rect(Some(context), self.clip());
            let [red, green, blue, alpha] = NATIVE_MARKDOWN_CODE_BACKGROUND;
            self.fill_color(context, [red, green, blue, alpha]);
            for line in &self.lines {
                if !self.visible(line.frame) {
                    continue;
                }
                // Block backgrounds use the same frame that established wrapping, baseline, and scroll height, without expanding document geometry here.
                use crate::native_surface::preview_paragraph::PreviewParagraphBackground as Background;
                match line.paragraph.background {
                    Some(Background::Code) => {
                        CGContext::fill_rect(Some(context), line.frame);
                        native_draw_returned = true;
                    }
                    Some(Background::Quote) => {
                        self.fill_color(context, [0.10, 0.12, 0.16, 1.0]);
                        CGContext::fill_rect(Some(context), line.frame);
                        self.fill_color(context, [0.28, 0.34, 0.44, 1.0]);
                        CGContext::fill_rect(
                            Some(context),
                            NSRect::new(
                                line.frame.origin,
                                NSSize::new(1.0, line.frame.size.height),
                            ),
                        );
                        CGContext::fill_rect(
                            Some(context),
                            NSRect::new(
                                NSPoint::new(
                                    line.frame.origin.x + line.frame.size.width - 1.0,
                                    line.frame.origin.y,
                                ),
                                NSSize::new(1.0, line.frame.size.height),
                            ),
                        );
                        // No duplicate horizontal borders between soft lines; only actual paragraph first/last lines own those edges.
                        let facts = line.handle.facts();
                        if facts.source_utf16.start == facts.paragraph_utf16.start {
                            CGContext::fill_rect(
                                Some(context),
                                NSRect::new(
                                    NSPoint::new(
                                        line.frame.origin.x,
                                        line.frame.origin.y + line.frame.size.height - 1.0,
                                    ),
                                    NSSize::new(line.frame.size.width, 1.0),
                                ),
                            );
                        }
                        if facts.source_utf16.end == facts.paragraph_utf16.end {
                            CGContext::fill_rect(
                                Some(context),
                                NSRect::new(
                                    line.frame.origin,
                                    NSSize::new(line.frame.size.width, 1.0),
                                ),
                            );
                        }
                        native_draw_returned = true;
                    }
                    Some(Background::Rule) => {
                        // The fixed rule stays in its actual 12 pt text slot; paragraph spacing/viewport edges cannot move it.
                        let low = line.frame.origin.y + line.geometry.after;
                        let y = low + line.geometry.text_height / 2.0;
                        self.fill_color(context, [0.28, 0.34, 0.44, 1.0]);
                        CGContext::fill_rect(
                            Some(context),
                            NSRect::new(
                                NSPoint::new(line.frame.origin.x, y),
                                NSSize::new(line.frame.size.width, 1.0),
                            ),
                        );
                        native_draw_returned = true;
                    }
                    None => {}
                }
                self.fill_color(context, [red, green, blue, alpha]);
                for part in &line.parts {
                    let facts = part.handle.facts();
                    let styles = facts.presentation;
                    if styles.contains(MarkdownPresentationStyles::HIDDEN_MARKER)
                        || !styles.contains(MarkdownPresentationStyles::INLINE_CODE)
                        || facts.x_advance_26_6 == 0
                    {
                        continue; // Hidden syntax, hard separators, and zero-width characters have no code-background area to paint.
                    }
                    self.require(current)?;
                    let metrics = self.palette[part.font]
                        .metrics()
                        .map_err(NativeMarkdownComposedViewportError::Font)?;
                    let start = line.baseline.x + facts.origin_x_26_6 as f64 / 64.0;
                    let end = start + facts.x_advance_26_6 as f64 / 64.0;
                    let baseline =
                        self.part_baseline(line, part).y + facts.origin_y_26_6 as f64 / 64.0;
                    CGContext::fill_rect(
                        Some(context),
                        NSRect::new(
                            NSPoint::new(start.min(end), baseline - metrics.descent),
                            NSSize::new((end - start).abs(), metrics.ascent + metrics.descent),
                        ),
                    ); // Use actual font ascent/descent for this part; selection paints afterward so code background cannot overwrite selected text.
                    native_draw_returned = true;
                }
            }
            drop(restore);
            self.require(current)
                .map_err(NativeMarkdownComposedViewportPaintCause::Window)
        })();
        result.map_err(|cause| NativeMarkdownComposedViewportPaintError {
            cause,
            native_draw_returned,
        })
    }

    /// Original font metrics/actual part advance determine decoration, without CTLine, text copies, or another style index.
    fn paint_decorations(
        &self,
        line: &Line,
        part: &Part,
        context: &CGContext,
        native_draw_returned: &mut bool,
        current: &dyn Fn() -> bool,
    ) -> std::result::Result<(), NativeMarkdownComposedViewportPaintCause> {
        self.require(current)?;
        let facts = part.handle.facts();
        let styles = facts.presentation;
        if styles.contains(MarkdownPresentationStyles::HIDDEN_MARKER)
            || (!styles.contains(MarkdownPresentationStyles::UNDERLINE)
                && !styles.contains(MarkdownPresentationStyles::LINK)
                && !styles.contains(MarkdownPresentationStyles::STRIKETHROUGH))
        {
            return Ok(()); // Hidden presentation also suppresses decorations; adjacent visible text retains its own part/foreground.
        }
        let metrics = self.palette[part.font]
            .metrics()
            .map_err(NativeMarkdownComposedViewportError::Font)?;
        let start = line.baseline.x + facts.origin_x_26_6 as f64 / 64.0;
        let end = start + facts.x_advance_26_6 as f64 / 64.0;
        let baseline = self.part_baseline(line, part).y + facts.origin_y_26_6 as f64 / 64.0;
        let thickness = metrics.underline_thickness;
        if start == end || thickness == 0.0 {
            return Ok(()); // Zero width/font thickness means no line ink; do not invent a minimum pixel size.
        }
        CGContext::save_g_state(Some(context));
        let restore = Restore(context);
        CGContext::clip_to_rect(Some(context), self.clip());
        self.ink_style(part, context); // Link underline shares actual text color; exiting this part restores caller foreground.
        // A part may have both decorations; use original style bits without inferring/parsing Markdown syntax.
        for (style, position) in [
            (
                MarkdownPresentationStyles::UNDERLINE,
                metrics.underline_position,
            ),
            (
                MarkdownPresentationStyles::STRIKETHROUGH,
                metrics.x_height / 2.0,
            ),
        ] {
            if styles.contains(style)
                || (style == MarkdownPresentationStyles::UNDERLINE
                    && styles.contains(MarkdownPresentationStyles::LINK))
            {
                self.require(current)?;
                CGContext::fill_rect(
                    Some(context),
                    NSRect::new(
                        NSPoint::new(start.min(end), baseline + position - thickness / 2.0),
                        NSSize::new((end - start).abs(), thickness),
                    ),
                ); // Retain Scene foreground over its already painted selection background, without overwriting backgrounds or changing global palette.
                *native_draw_returned = true; // Later revocation must preserve the actual returned draw fact instead of erasing it with an error.
            }
        }
        drop(restore);
        self.require(current)?;
        Ok(())
    }

    /// Paint actual window glyphs/part decorations; Scene still composes selection backgrounds, carets, and paragraph appearance.
    ///
    /// # Errors
    /// Excess synchronous work returns Pending before first Native draw; other failures retain whether actual drawing returned.
    pub fn paint(
        &self,
        context: &CGContext,
        current: &dyn Fn() -> bool,
    ) -> std::result::Result<
        NativeMarkdownComposedViewportPaintReceipt,
        NativeMarkdownComposedViewportPaintError,
    > {
        let mut native_draw_returned = false;
        let result = (|| -> std::result::Result<_, NativeMarkdownComposedViewportPaintCause> {
            self.require(current)?;
            // Check synchronous budget before first draw; large graphemes/lines cannot paint a prefix and claim a complete frame.
            if self.glyphs > NATIVE_MARKDOWN_COMPOSED_VIEWPORT_GLYPHS {
                return Err(NativeMarkdownComposedViewportError::Pending.into());
            }
            // Modify only the caller's unpublished candidate context, clipped to original Scene's actual bounded viewport.
            CGContext::save_g_state(Some(context));
            let restore = Restore(context);
            CGContext::clip_to_rect(Some(context), self.clip());
            let mut receipt = NativeMarkdownComposedViewportPaintReceipt {
                glyphs: 0,
                batches: 0,
                missing_glyphs: 0,
            };
            // Bounded overscan lines may stay in metadata; send only lines intersecting original clip to Native.
            for line in &self.lines {
                if !self.visible(line.frame) {
                    continue;
                }
                for part in &line.parts {
                    if part.handle.facts().kind != MarkdownComposedLinePartKind::Glyphs {
                        continue; // Actual Tabs/separators contribute layout only; never send missing-glyph placeholders to CTFont.
                    }
                    let facts = part.handle.facts();
                    CGContext::save_g_state(Some(context));
                    let part_restore = Restore(context);
                    self.ink_style(part, context);
                    let (mut pen_x, mut pen_y) = (facts.origin_x_26_6, facts.origin_y_26_6);
                    let mut start = 0;
                    // Start pen at original part visual origin; subsequent batches use actual receipt advances rather than reset to line start.
                    while start < facts.glyphs {
                        self.require(current)?;
                        let end = start
                            .saturating_add(NATIVE_MARKDOWN_PAGED_GLYPH_BATCH)
                            .min(facts.glyphs);
                        let font = &self.palette[part.font];
                        let font_authority = font.native().core_font().authority();
                        let painted = paint_composed_glyphs(
                            &self.output,
                            &part.handle,
                            NativeMarkdownPagedGlyphPaintRequest {
                                source: &self.source(),
                                font_authority: &font_authority,
                                native_font: font.native(),
                                glyph_range: start..end,
                                origin: NativeMarkdownPagedGlyphOrigin {
                                    baseline: self.part_baseline(line, part),
                                    pen_x_26_6: pen_x,
                                    pen_y_26_6: pen_y,
                                },
                            },
                            context,
                            &|| self.require(current).is_ok(),
                        );
                        // Revocation after each actual draw preserves its effect, never implying an untouched candidate that may be replayed.
                        native_draw_returned |= painted
                            .as_ref()
                            .map_or_else(|error| error.native_draw_returned, |_| true);
                        self.require(current)?; // Preserve returned draw first, then give original Source failure priority.
                        let painted =
                            painted.map_err(NativeMarkdownComposedViewportPaintCause::Glyph)?;
                        pen_x = painted.final_pen_x_26_6;
                        pen_y = painted.final_pen_y_26_6;
                        receipt.glyphs += end - start;
                        receipt.batches += 1;
                        receipt.missing_glyphs += painted.missing_glyphs;
                        start = end;
                    }
                    if facts.origin_x_26_6.checked_add(facts.x_advance_26_6) != Some(pen_x)
                        || facts.origin_y_26_6.checked_add(facts.y_advance_26_6) != Some(pen_y)
                    {
                        return Err(NativeMarkdownComposedViewportPaintCause::InconsistentAdvance);
                    } // Incomplete actual advance invalidates the whole candidate; do not skip glyphs or fill gaps with estimated width.
                    drop(part_restore); // Hidden alpha belongs to this part only, without affecting later visible text/selection.
                    self.paint_decorations(
                        line,
                        part,
                        context,
                        &mut native_draw_returned,
                        current,
                    )?;
                }
            }
            drop(restore); // Restore this layer's Native state then check original Source; returning a receipt does not publish candidate pixels.
            self.require(current)?;
            Ok(receipt)
        })();
        result.map_err(|cause| NativeMarkdownComposedViewportPaintError {
            cause,
            native_draw_returned,
        })
    }
}
