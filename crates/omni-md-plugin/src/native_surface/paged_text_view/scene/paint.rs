//! 消费式CPU绘图协议；只有真实有界producer可以签完整tile完成票。

use super::*;
use crate::native_surface::paged_glyph_paint::{
    NativeMarkdownPagedGlyphPaintCause, NativeMarkdownPagedGlyphPaintReceipt,
    NativeMarkdownPagedGlyphPaintRequest, paint_paged_spool_glyphs,
};
use objc2_core_foundation::{CFRetained, Type};
use omni_markdown_core::{MarkdownShapedSpool, MarkdownShapedSpoolAuthority};

#[derive(Clone, Copy, Debug)]
pub struct NativeMarkdownPagedScenePaintBudget {
    pub glyphs: usize,
    pub metadata_records: usize,
}
impl Default for NativeMarkdownPagedScenePaintBudget {
    fn default() -> Self {
        Self {
            glyphs: 256,
            metadata_records: 32,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NativeMarkdownPagedScenePaintWork {
    pub glyphs: usize,
    pub batches: usize,
    pub metadata_records: usize,
}

/// 每个Pending都归还原状态；失败消费它，调用方不能重放失败的原生绘图。
pub trait NativeMarkdownPagedScenePaintSession {
    fn step(
        self: Box<Self>,
        budget: NativeMarkdownPagedScenePaintBudget,
        current: &dyn Fn() -> bool,
    ) -> Result<NativeMarkdownPagedScenePaintStep>;
}
pub struct NativeMarkdownPagedScenePaintStep {
    pub work: NativeMarkdownPagedScenePaintWork,
    pub outcome: NativeMarkdownPagedScenePaintOutcome,
}
pub enum NativeMarkdownPagedScenePaintOutcome {
    Pending(Box<dyn NativeMarkdownPagedScenePaintSession>),
    Complete(NativeMarkdownPagedScenePaintCompletion),
}

/// 私有完成票线性持有原context，防止地址复用ABA；消费后才归还这份真实像素claim。
/// SceneKey仍是弱身份，不延长窗口、Source或Font生命期。
pub struct NativeMarkdownPagedScenePaintCompletion {
    key: Box<NativeMarkdownPagedSceneKey>,
    context: CFRetained<CGContext>,
    work: NativeMarkdownPagedScenePaintWork,
}
impl NativeMarkdownPagedScenePaintCompletion {
    #[must_use]
    pub fn permits(&self, key: &NativeMarkdownPagedSceneKey, context: &CGContext) -> bool {
        self.key.same(key) && std::ptr::eq::<CGContext>(&*self.context, context)
    }
    #[must_use]
    pub fn work(&self) -> NativeMarkdownPagedScenePaintWork {
        self.work
    }

    /// 接受真实组合器完成票，而非调用者自报的glyph计数或成功布尔值。
    #[cfg(feature = "native-paged-production")]
    pub fn from_composed(
        completion: crate::native_surface::paged_line_viewport::NativeMarkdownComposedViewportPaintCompletion,
        key: &NativeMarkdownPagedSceneKey,
        context: &CGContext,
    ) -> Result<Self> {
        if !completion.permits(key, context) {
            return Err(super::super::NativeMarkdownPagedTextViewError::Revoked);
        }
        let actual = completion.work();
        Ok(Self {
            key: Box::new(key.clone()),
            context: completion.context_claim(),
            work: NativeMarkdownPagedScenePaintWork {
                glyphs: actual.glyphs,
                batches: actual.batches,
                metadata_records: actual.metadata_records,
            },
        })
    }

    /// 原Scene消费全部真实Text/Cell完成票后转交同一context；空格背景也在该同步栈实际绘完。
    /// 此入口仅供native_surface内部装配，公开调用者仍必须提交真实组合器完成票。
    pub(in crate::native_surface) fn from_native_composite(
        key: &NativeMarkdownPagedSceneKey,
        context: CFRetained<CGContext>,
        work: NativeMarkdownPagedScenePaintWork,
    ) -> Self {
        Self {
            key: Box::new(key.clone()),
            context,
            work,
        }
    }

    /// 有限旧spool适配直接执行唯一真实批次，不能用外部receipt或declaredLimit授权任意scene.paint。
    /// 原输出必须完整不超过256glyph；长正文必须走可续组合器，不会从此入口循环全文。
    pub fn paint_finite_spool(
        spool: &mut MarkdownShapedSpool,
        output: &MarkdownShapedSpoolAuthority,
        request: NativeMarkdownPagedGlyphPaintRequest<'_>,
        key: &NativeMarkdownPagedSceneKey,
        context: &CGContext,
        current: &dyn Fn() -> bool,
    ) -> Result<(Self, NativeMarkdownPagedGlyphPaintReceipt)> {
        let count = spool.glyph_count();
        let actual_font = request.native_font.core_font().authority();
        for font in [request.font_authority, &actual_font] {
            spool
                .read_batch(output, request.source, font, count..count, &mut [])
                .map_err(|cause| match cause {
                    omni_markdown_core::MarkdownShapedSpoolError::Source(cause)
                    | omni_markdown_core::MarkdownShapedSpoolError::Shaping(
                        omni_markdown_core::MarkdownShapingError::Source(cause),
                    ) => super::super::NativeMarkdownPagedTextViewError::Document(cause),
                    other => super::super::NativeMarkdownPagedTextViewError::ShapedSpool(other),
                })?;
        } // 实际原Source/Font/输出健康先于调用者范围拒绝；没有Native或current跨Reader借用。
        if count == 0
            || count > 256
            || request.glyph_range != (0..count)
            || request.source != &key.source
        {
            return Err(super::super::NativeMarkdownPagedTextViewError::InvalidRange);
        } // 预算和范围来自原输出真实profile，不接受调用者把长spool伪报成小批。
        let receipt = paint_paged_spool_glyphs(spool, output, request, context, current).map_err(
            |error| match error.cause {
                NativeMarkdownPagedGlyphPaintCause::Revoked => {
                    super::super::NativeMarkdownPagedTextViewError::Revoked
                }
                NativeMarkdownPagedGlyphPaintCause::Spool(
                    omni_markdown_core::MarkdownShapedSpoolError::Source(error),
                )
                | NativeMarkdownPagedGlyphPaintCause::Shaping(
                    omni_markdown_core::MarkdownShapingError::Source(error),
                ) => super::super::NativeMarkdownPagedTextViewError::Document(error),
                NativeMarkdownPagedGlyphPaintCause::Spool(error) => {
                    super::super::NativeMarkdownPagedTextViewError::ShapedSpool(error)
                }
                other => super::super::NativeMarkdownPagedTextViewError::Raster(other.to_string()),
            },
        )?;
        if !current() {
            return Err(super::super::NativeMarkdownPagedTextViewError::Revoked);
        }
        Ok((
            Self {
                key: Box::new(key.clone()),
                context: context.retain(),
                work: NativeMarkdownPagedScenePaintWork {
                    glyphs: count,
                    batches: 1,
                    metadata_records: 1,
                },
            },
            receipt,
        ))
    }
}
