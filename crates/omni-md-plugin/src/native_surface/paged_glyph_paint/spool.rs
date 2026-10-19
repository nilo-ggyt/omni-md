//! 主线程只读消费原worker封存的字形文件；不把可变Job或NativeFont跨线程移交。

use objc2_core_graphics::CGContext;
use omni_markdown_core::{MarkdownShapedGlyph, MarkdownShapedSpool, MarkdownShapedSpoolAuthority};

use super::{
    NATIVE_MARKDOWN_PAGED_GLYPH_BATCH, NativeMarkdownPagedGlyphPaintCause,
    NativeMarkdownPagedGlyphPaintError, NativeMarkdownPagedGlyphPaintReceipt,
    NativeMarkdownPagedGlyphPaintRequest, PreparedBatch, Result, batch_count, validate_native_font,
};

/// 封存文件的每次读取都绑定同一输出nonce；相同Source/Font的新任务不能替代原文件。
struct SpoolProof<'a, 'b> {
    output: &'a MarkdownShapedSpoolAuthority,
    request: &'a NativeMarkdownPagedGlyphPaintRequest<'b>,
    native_font_authority: omni_markdown_core::MarkdownLayoutFontAuthority,
    glyph_count: usize,
}

impl SpoolProof<'_, '_> {
    fn current(
        &self,
        spool: &mut MarkdownShapedSpool,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<()> {
        if !current() {
            return Err(NativeMarkdownPagedGlyphPaintCause::Revoked);
        }
        // 真EOF空批次没有pread；仍核同一输出、Source健康、取消位与精确Font payload。
        // 同时核请求Font和实际CTFont对应的CoreFont，不能借相同PSName冒充同一face权限。
        for font in [self.request.font_authority, &self.native_font_authority] {
            spool.read_batch(
                self.output,
                self.request.source,
                font,
                self.glyph_count..self.glyph_count,
                &mut [],
            )?;
        }
        if !current() {
            return Err(NativeMarkdownPagedGlyphPaintCause::Revoked);
        }
        // 原current可以同步发现源回收失败；返回前再核原票，不领取较新的输出或Source。
        spool.read_batch(
            self.output,
            self.request.source,
            self.request.font_authority,
            self.glyph_count..self.glyph_count,
            &mut [],
        )?;
        Ok(())
    }
}

/// 将原worker已完整seal的指定字形批次画到调用者的真实context。
///
/// 每次最多256glyph，按原视觉顺序消费一次真实`read_batch`，全部预检后才唯一draw。
/// `output`必须来自该spool的原任务nonce；调用者须结束所有Doc/RefCell借用、保留原前台票，
/// 并在任何Err后丢弃本次候选像素。本函数不提供软行、clip、选择或视口Ready权限。
///
/// # Errors
/// 超量、失权、原Source/Font/输出不符或文件IO均明确拒绝；draw后撤权保留实际绘制返回事实。
pub fn paint_paged_spool_glyphs(
    spool: &mut MarkdownShapedSpool,
    output: &MarkdownShapedSpoolAuthority,
    request: NativeMarkdownPagedGlyphPaintRequest<'_>,
    context: &CGContext,
    current: &(impl Fn() -> bool + ?Sized),
) -> std::result::Result<NativeMarkdownPagedGlyphPaintReceipt, NativeMarkdownPagedGlyphPaintError> {
    let mut native_draw_returned = false;
    let result = (|| -> Result<NativeMarkdownPagedGlyphPaintReceipt> {
        // 大范围在读取任何glyph前整批拒绝，不以截断或内部循环画完整长run。
        let count = batch_count(&request.glyph_range)?;
        let proof = SpoolProof {
            output,
            request: &request,
            native_font_authority: request.native_font.core_font().authority(),
            glyph_count: spool.glyph_count(),
        };
        proof.current(spool, current)?;
        if request.glyph_range.end > proof.glyph_count {
            return Err(NativeMarkdownPagedGlyphPaintCause::InvalidRange);
        }
        if !request.native_font.accepts_shaping_size(spool.points()) {
            return Err(NativeMarkdownPagedGlyphPaintCause::FontSizeMismatch);
        }
        validate_native_font(&request, &mut || proof.current(spool, current))?;
        let mut records = [MarkdownShapedGlyph {
            glyph_id: 0,
            source_utf16_cluster: 0,
            flags: 0,
            x_advance: 0,
            y_advance: 0,
            x_offset: 0,
            y_offset: 0,
        }; NATIVE_MARKDOWN_PAGED_GLYPH_BATCH];
        // 整批是固定栈候选，read_batch错误时全部弃用，不向CoreText交付成功前缀。
        let read = spool.read_batch(
            output,
            request.source,
            request.font_authority,
            request.glyph_range.clone(),
            &mut records[..count],
        )?;
        if read != count {
            return Err(NativeMarkdownPagedGlyphPaintCause::InvalidRange);
        }
        proof.current(spool, current)?;
        let mut batch = PreparedBatch::new(&request, spool.source_range());
        for glyph in records[..count].iter().copied() {
            // 这里没有Native调用或正文读取；共用Job路径相同的编号/26.6/flags预检。
            batch.push(&request, glyph)?;
        }
        batch.draw(
            &request,
            context,
            &mut || proof.current(spool, current),
            &mut native_draw_returned,
        )
    })();
    result.map_err(|cause| NativeMarkdownPagedGlyphPaintError {
        cause,
        native_draw_returned,
    })
}
