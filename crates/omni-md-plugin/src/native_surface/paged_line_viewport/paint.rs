//! 同窗口真实part按≤256glyph绘制；超同步预算不画前缀，失败由调用者丢弃整个候选。

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
    /// 有色墨迹与背景共享原窗口的一份 sRGB 输入空间；不会借目标 bitmap 猜源颜色。
    /// 调用均处于原绘图 save/restore 内，普通文字没有专属着色时仍继承调用者原 fill。
    fn fill_color(&self, context: &CGContext, color: [f64; 4]) {
        CGContext::set_fill_color_space(Some(context), Some(&self.paint_space));
        // SAFETY: 当前命名 sRGB 恰有 RGB 三分量；栈数组另含 alpha，原 API 返回后不保该指针。
        unsafe { CGContext::set_fill_color(Some(context), color.as_ptr()) };
    }

    /// 只改变当前part的墨迹状态；真实glyph/advance不变，颜色与原TextKit链接策略一致。
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
            // 引用中的强调、等宽代码仍使用各自真实Font；只按原颜色合同降低正文墨迹亮度。
            // 可见标记与链接已在上方领取自己的颜色，不能被容器样式覆盖。
            let [red, green, blue, alpha] = NATIVE_MARKDOWN_QUOTE_COLOR;
            self.fill_color(context, [red, green, blue, alpha]);
        } // alpha同时覆盖彩色字形；显露语法不借LINK位着成可点击内容，也不替换真实Font。
    }

    /// 已封存窗口的代码底色先于Scene选区绘制；只遍历原有限parts，不读正文或建立字形副本。
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
                // 块背景只消费已经决定软换行、baseline 和滚动高度的同一行框；不能在这里扩张正文。
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
                        // 软行之间没有重复横框；只有同一源段真正首末行才拥有对应边缘。
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
                        // 固定细线只位于真实 12 点文字槽内；段距和 viewport 边缘不会移动其位置。
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
                        continue; // 隐藏语法、硬分隔和零宽字符都没有可绘制的代码底色面积。
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
                    ); // 底色采用该part原字体真实上下沿，选区稍后覆盖，不能把已选文字涂回代码底色。
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

    /// 原字体度量和实际part视觉宽度决定装饰；不会创建CTLine、正文副本或另一个样式索引。
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
            return Ok(()); // 真实隐藏权限也覆盖装饰；相邻可见文字保留自己的独立part和原前景。
        }
        let metrics = self.palette[part.font]
            .metrics()
            .map_err(NativeMarkdownComposedViewportError::Font)?;
        let start = line.baseline.x + facts.origin_x_26_6 as f64 / 64.0;
        let end = start + facts.x_advance_26_6 as f64 / 64.0;
        let baseline = self.part_baseline(line, part).y + facts.origin_y_26_6 as f64 / 64.0;
        let thickness = metrics.underline_thickness;
        if start == end || thickness == 0.0 {
            return Ok(()); // 零宽或原字体无厚度时确实没有线墨迹，不捏造像素最小值。
        }
        CGContext::save_g_state(Some(context));
        let restore = Restore(context);
        CGContext::clip_to_rect(Some(context), self.clip());
        self.ink_style(part, context); // 链接线与其真实文字同色；本part退出即还原调用者前景。
        // 同一个part可同时拥有两种装饰；只消费原位，不推断或解析Markdown语法。
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
                ); // 继承本Scene原前景色，选区背景已经由Scene先画；不覆盖背景或调整全局palette。
                *native_draw_returned = true; // 后继撤权必须保留真实draw已返回的事实，不能用Err抹掉。
            }
        }
        drop(restore);
        self.require(current)?;
        Ok(())
    }

    /// 画本窗口真实glyph和原part装饰；选区背景、caret和段落外观仍由Scene组合。
    ///
    /// # Errors
    /// 超固定同步预算在首个Native draw前返回Pending；其余错误保留是否已有真实draw事实。
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
            // 同步预算先于首个draw检查；超大GCB/行不能画一个前缀后伪称完整frame。
            if self.glyphs > NATIVE_MARKDOWN_COMPOSED_VIEWPORT_GLYPHS {
                return Err(NativeMarkdownComposedViewportError::Pending.into());
            }
            // 只改调用者尚未发布的候选context，clip固定为原Scene的真实有限viewport。
            CGContext::save_g_state(Some(context));
            let restore = Restore(context);
            CGContext::clip_to_rect(Some(context), self.clip());
            let mut receipt = NativeMarkdownComposedViewportPaintReceipt {
                glyphs: 0,
                batches: 0,
                missing_glyphs: 0,
            };
            // 有限overscan行可以留在metadata窗口，只有与原clip相交的行实际送入Native。
            for line in &self.lines {
                if !self.visible(line.frame) {
                    continue;
                }
                for part in &line.parts {
                    if part.handle.facts().kind != MarkdownComposedLinePartKind::Glyphs {
                        continue; // 真Tab/分隔符只贡献布局；绝不向CTFont发送缺字占位。
                    }
                    let facts = part.handle.facts();
                    CGContext::save_g_state(Some(context));
                    let part_restore = Restore(context);
                    self.ink_style(part, context);
                    let (mut pen_x, mut pen_y) = (facts.origin_x_26_6, facts.origin_y_26_6);
                    let mut start = 0;
                    // pen从原part视觉origin开始，后继批次沿真实receipt累计，不重置到行首。
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
                        // 每个真实draw后的失权都保留事实，不能解释为未修改候选context再重放。
                        native_draw_returned |= painted
                            .as_ref()
                            .map_or_else(|error| error.native_draw_returned, |_| true);
                        self.require(current)?; // 先保留实际draw事实，再优先暴露原Source故障。
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
                    } // 未覆盖全部真实advance时整候选作废，不能跳字或用估计宽度补齐。
                    drop(part_restore); // 隐藏alpha只属于这个part，不能污染后继可见正文或选区。
                    self.paint_decorations(
                        line,
                        part,
                        context,
                        &mut native_draw_returned,
                        current,
                    )?;
                }
            }
            drop(restore); // 配平本层Native状态后仍核原Source，返回receipt不等于发布候选像素。
            self.require(current)?;
            Ok(receipt)
        })();
        result.map_err(|cause| NativeMarkdownComposedViewportPaintError {
            cause,
            native_draw_returned,
        })
    }
}
