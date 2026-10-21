//! Consuming CPU paint protocol: only an actual bounded producer can certify complete tile output.

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

/// Pending returns its original state; failure consumes it, so callers cannot replay failed Native painting.
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

/// The private completion linearly owns its original context to prevent address-reuse ABA; consuming it returns that actual pixel claim.
/// SceneKey remains weak identity, retaining neither window, Source, nor font lifetime.
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

    /// Accept actual composer completion, not caller-declared glyph counts or a success boolean.
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

    /// Scene transfers the same context after consuming actual Text/Cell completions; empty-cell backgrounds also finish on that synchronous stack.
    /// Only native_surface assembly uses this entry; public callers still supply actual composer completion.
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

    /// The finite spool adapter executes one actual batch; an external receipt/declaredLimit cannot authorize arbitrary scene.paint.
    /// Complete original output has at most 256 glyphs; long text uses the resumable composer rather than looping through the document here.
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
        } // Actual original Source/font/output health precedes caller-range rejection; no Native/current crosses a reader borrow.
        if count == 0
            || count > 256
            || request.glyph_range != (0..count)
            || request.source != &key.source
        {
            return Err(super::super::NativeMarkdownPagedTextViewError::InvalidRange);
        } // Budget/range come from the actual output profile; callers cannot present a long spool as a small batch.
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
