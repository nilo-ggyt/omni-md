//! 唯一已封存行目录的有界绘制；任何Native调用与重入检查都在Reader借用之外。

use std::cell::RefCell;

use objc2_core_graphics::CGContext;
use omni_markdown_core::{
    MarkdownComposedLinePart, MarkdownComposedLinePartKind, MarkdownComposedLines,
    MarkdownShapedGlyph,
};

use super::{
    NATIVE_MARKDOWN_PAGED_GLYPH_BATCH, NativeMarkdownPagedGlyphPaintCause as Cause,
    NativeMarkdownPagedGlyphPaintError, NativeMarkdownPagedGlyphPaintReceipt,
    NativeMarkdownPagedGlyphPaintRequest, PreparedBatch, Result, batch_count, validate_native_font,
};

/// 有界空批只检查原输出/part/Source/Font；借用归还以后才允许运行可重入的current。
fn require(
    output: &RefCell<MarkdownComposedLines>,
    part: &MarkdownComposedLinePart,
    request: &NativeMarkdownPagedGlyphPaintRequest<'_>,
) -> Result<()> {
    let registered = part.font_registration().lease()?;
    if !registered.font().permits(request.font_authority)
        || !registered
            .font()
            .permits(&request.native_font.core_font().authority())
    {
        return Err(Cause::InvalidGlyph);
    } // 同名或同字号的新capture没有原目录的Font身份，不能解释它的glyph编号。
    let mut lines = output.try_borrow_mut().map_err(|_| Cause::OutputBusy)?;
    lines.require(request.source)?;
    lines.read_batch(part, 0..0, &mut [])?;
    Ok(())
}

/// 一个同步调用只画一个真实part的≤256个字形；不会跨part推断来源或临时排版。
///
/// `output`必须是原组合器完整封存的唯一Reader，`part`必须由它真实LineEnd产生。
/// `origin`已包含该part之前的实际视觉pen；调用者负责同票clip、装饰和候选像素发布。
/// 所有Err均丢弃本轮候选；draw已经返回时错误保留实际绘制事实，不许重放编辑。
///
/// # Errors
/// 失权、重入借用、来源/Font/part不符、批次或坐标无效以及真实IO失败均整批拒绝。
pub fn paint_composed_glyphs(
    output: &RefCell<MarkdownComposedLines>,
    part: &MarkdownComposedLinePart,
    request: NativeMarkdownPagedGlyphPaintRequest<'_>,
    context: &CGContext,
    current: &(impl Fn() -> bool + ?Sized),
) -> std::result::Result<NativeMarkdownPagedGlyphPaintReceipt, NativeMarkdownPagedGlyphPaintError> {
    let mut native_draw_returned = false;
    let result = (|| -> Result<NativeMarkdownPagedGlyphPaintReceipt> {
        let mut check = || -> Result<()> {
            if !current() {
                return Err(Cause::Revoked);
            }
            require(output, part, &request)?;
            if !current() {
                return Err(Cause::Revoked);
            }
            require(output, part, &request) // 后一次current也可能撤Source或Font；不能用较新的票补签。
        };
        check()?;
        if part.facts().kind != MarkdownComposedLinePartKind::Glyphs {
            return Err(Cause::NonGlyphPart); // Tab和段分隔符只贡献真实布局，不向CTFont提交.notdef。
        }
        let count = batch_count(&request.glyph_range)?;
        if !request
            .native_font
            .accepts_shaping_size(part.facts().points)
        {
            return Err(Cause::FontSizeMismatch);
        }
        validate_native_font(&request, &mut check)?;
        let mut records = [MarkdownShapedGlyph {
            glyph_id: 0,
            source_utf16_cluster: 0,
            flags: 0,
            x_advance: 0,
            y_advance: 0,
            x_offset: 0,
            y_offset: 0,
        }; NATIVE_MARKDOWN_PAGED_GLYPH_BATCH];
        {
            // 这里仅执行纯Core pread；不调用current、Native getter或delegate。
            let mut lines = output.try_borrow_mut().map_err(|_| Cause::OutputBusy)?;
            lines.require(request.source)?;
            lines.read_batch(part, request.glyph_range.clone(), &mut records[..count])?;
        }
        check()?; // 固定栈候选已完整读取；若重入撤权，不向Native发布任何成功前缀。
        let mut batch = PreparedBatch::new(&request, part.facts().source_utf16.clone());
        for glyph in records[..count].iter().copied() {
            batch.push(&request, glyph)?;
        } // 与旧Job/spool共用全部glyph编号、26.6、offset和advance验证。
        batch.draw(&request, context, &mut check, &mut native_draw_returned)
    })();
    result.map_err(|cause| NativeMarkdownPagedGlyphPaintError {
        cause,
        native_draw_returned,
    })
}
