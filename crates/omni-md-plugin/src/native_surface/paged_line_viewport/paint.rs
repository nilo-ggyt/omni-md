//! 同窗口真实part按≤256glyph绘制；超同步预算不画前缀，失败由调用者丢弃整个候选。

use super::*;
use crate::native_surface::paged_glyph_paint::{
    NATIVE_MARKDOWN_PAGED_GLYPH_BATCH, NativeMarkdownPagedGlyphOrigin,
    NativeMarkdownPagedGlyphPaintError, NativeMarkdownPagedGlyphPaintRequest,
    paint_composed_glyphs,
};
use objc2_core_graphics::CGContext;
use omni_markdown_core::MarkdownComposedLinePartKind;

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
    /// 仅画本窗口的真实glyph；背景、装饰和段落样式由后继组合层提供。
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
            CGContext::clip_to_rect(Some(context), self.key.request.viewport);
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
                                source: &self.key.source,
                                font_authority: &font_authority,
                                native_font: font.native(),
                                glyph_range: start..end,
                                origin: NativeMarkdownPagedGlyphOrigin {
                                    baseline: line.baseline,
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
