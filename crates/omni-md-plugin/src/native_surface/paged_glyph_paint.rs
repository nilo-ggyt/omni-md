//! 只绘制同一Ready任务的一批真实字形；不创建文字、段落、软行或原生视图。

use std::{ops::Range, ptr::NonNull};

use objc2_core_foundation::{CGAffineTransform, CGPoint};
use objc2_core_graphics::CGContext;
use omni_markdown_core::{
    MarkdownLayoutFontAuthority, MarkdownLayoutTextAuthority, MarkdownShapedGlyph,
    MarkdownShapedSpoolError, MarkdownShapingError, MarkdownShapingJob, MarkdownShapingStatus,
};
use thiserror::Error;

use super::paged_fonts::NativeMarkdownPagedFont;

mod spool;
pub use spool::paint_paged_spool_glyphs;
#[cfg(feature = "native-paged-production")]
mod composed;
#[cfg(feature = "native-paged-production")]
pub use composed::paint_composed_glyphs;

/// 一次同步调用的固定栈预算；较长可见结果须由调度者保存原票并明确分批。
pub const NATIVE_MARKDOWN_PAGED_GLYPH_BATCH: usize = 256;
const MAX_EXACT_26_6: i64 = (1i64 << 53) - 1;
const MAX_POINT_MAGNITUDE: f64 = (1u64 << 46) as f64;

#[derive(Clone, Copy, Debug)]
/// baseline属于调用者当前CGContext的y-up用户坐标；pen沿shaper实际视觉顺序累加。
pub struct NativeMarkdownPagedGlyphOrigin {
    pub baseline: CGPoint,
    pub pen_x_26_6: i64,
    pub pen_y_26_6: i64,
}

/// 所有引用仅借本次调用；两个authority必须来自创建同一个job时的原owner。
pub struct NativeMarkdownPagedGlyphPaintRequest<'a> {
    pub source: &'a MarkdownLayoutTextAuthority,
    pub font_authority: &'a MarkdownLayoutFontAuthority,
    pub native_font: &'a NativeMarkdownPagedFont,
    pub glyph_range: Range<usize>,
    pub origin: NativeMarkdownPagedGlyphOrigin,
}

#[derive(Debug, Error)]
pub enum NativeMarkdownPagedGlyphPaintCause {
    #[error("原字形绘制权限已经撤销")]
    Revoked,
    #[error("单次字形范围必须非空且不超过256项")]
    BatchLimit,
    #[error("字形范围不属于该Ready任务")]
    InvalidRange,
    #[error("字形任务尚未Ready")]
    Pending,
    #[error("原生字体尺寸与该任务不一致")]
    FontSizeMismatch,
    #[error("原生字体含本任务尚未建模的变换")]
    FontMatrixUnsupported,
    #[error("字形编号不能由同一个原生字体绘制")]
    InvalidGlyph,
    #[error("字形坐标溢出、非有限或超出26.6可精确表示范围")]
    InvalidCoordinate,
    #[error("无法读取原Ready字形：{0}")]
    Shaping(#[from] MarkdownShapingError),
    #[error("无法读取原sealed字形：{0}")]
    Spool(#[from] MarkdownShapedSpoolError),
    #[cfg(feature = "native-paged-production")]
    #[error("原分页行输出正在被使用，不能重入读取")]
    OutputBusy,
    #[cfg(feature = "native-paged-production")]
    #[error("无法读取原分页行输出：{0}")]
    Composed(#[from] omni_markdown_core::MarkdownLineComposeError),
    #[cfg(feature = "native-paged-production")]
    #[error("这个原part是布局控制，不能当作字形批次绘制")]
    NonGlyphPart,
}

#[derive(Debug, Error)]
#[error("{cause}；原生绘制已返回={native_draw_returned}")]
/// draw返回以后失权仍是失败，但调用者必须丢弃候选像素，不能谎称目标context未改变。
pub struct NativeMarkdownPagedGlyphPaintError {
    pub cause: NativeMarkdownPagedGlyphPaintCause,
    pub native_draw_returned: bool,
}

#[derive(Debug)]
/// 只证明指定批次已实际调用绘制且末端同票；不证明墨迹、clip或软行布局Ready。
pub struct NativeMarkdownPagedGlyphPaintReceipt {
    pub glyph_range: Range<usize>,
    pub source_utf16_range: Range<usize>,
    pub first_source_cluster: usize,
    pub last_source_cluster: usize,
    pub flags_or: u32,
    pub missing_glyphs: usize,
    pub final_pen_x_26_6: i64,
    pub final_pen_y_26_6: i64,
}

type Result<T> = std::result::Result<T, NativeMarkdownPagedGlyphPaintCause>;

struct Proof<'a, 'b> {
    request: &'a NativeMarkdownPagedGlyphPaintRequest<'b>,
    native_font_authority: MarkdownLayoutFontAuthority,
    glyph_count: usize,
}

impl Proof<'_, '_> {
    fn current(
        &self,
        job: &mut MarkdownShapingJob,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<()> {
        if !current() {
            return Err(NativeMarkdownPagedGlyphPaintCause::Revoked);
        }
        // 查询真实EOF只执行原job的权限/健康门，不读取字形，也不借名字重建另一Font。
        if job
            .glyph(
                self.request.source,
                self.request.font_authority,
                self.glyph_count,
            )?
            .is_some()
            || job
                .glyph(
                    self.request.source,
                    &self.native_font_authority,
                    self.glyph_count,
                )?
                .is_some()
        {
            return Err(NativeMarkdownPagedGlyphPaintCause::InvalidRange);
        }
        if !current() {
            return Err(NativeMarkdownPagedGlyphPaintCause::Revoked);
        }
        // 最后一次current可以同步发现源回收失败；再核同一原身份，绝不重捕后继票。
        if job
            .glyph(
                self.request.source,
                self.request.font_authority,
                self.glyph_count,
            )?
            .is_some()
        {
            return Err(NativeMarkdownPagedGlyphPaintCause::InvalidRange);
        }
        Ok(())
    }
}

/// 配对归还本调用保存的context状态；拒绝后也须撤自己的保存帧，不能留下栈污染。
struct ContextRestore<'a> {
    context: &'a CGContext,
    text_matrix: CGAffineTransform,
    text_position: CGPoint,
}

impl Drop for ContextRestore<'_> {
    fn drop(&mut self) {
        CGContext::restore_g_state(Some(self.context));
        // CoreText公开声明会改text matrix；它和text position另行原样恢复，不假定包含于GState。
        // 这里只履行原保存帧的清理，不重画、不获取几何，也不把已撤权的候选发布出去。
        CGContext::set_text_matrix(Some(self.context), self.text_matrix);
        CGContext::set_text_position(
            Some(self.context),
            self.text_position.x,
            self.text_position.y,
        );
    }
}

fn axis_position(baseline: f64, pen: i64, offset: i32) -> Result<f64> {
    let positioned = pen
        .checked_add(i64::from(offset))
        .filter(|value| value.unsigned_abs() <= MAX_EXACT_26_6 as u64)
        .ok_or(NativeMarkdownPagedGlyphPaintCause::InvalidCoordinate)?;
    if !baseline.is_finite() || baseline.abs() >= MAX_POINT_MAGNITUDE {
        return Err(NativeMarkdownPagedGlyphPaintCause::InvalidCoordinate);
    }
    let result = baseline + positioned as f64 / 64.0;
    if !result.is_finite() || result.abs() >= MAX_POINT_MAGNITUDE {
        return Err(NativeMarkdownPagedGlyphPaintCause::InvalidCoordinate);
    }
    Ok(result)
}

fn advance(pen: i64, amount: i32) -> Result<i64> {
    pen.checked_add(i64::from(amount))
        .filter(|value| value.unsigned_abs() <= MAX_EXACT_26_6 as u64)
        .ok_or(NativeMarkdownPagedGlyphPaintCause::InvalidCoordinate)
}

/// 两条来源只共用批次数学与同步绘制；身份校验仍由各自原owner逐次执行。
struct PreparedBatch {
    glyphs: [u16; NATIVE_MARKDOWN_PAGED_GLYPH_BATCH],
    positions: [CGPoint; NATIVE_MARKDOWN_PAGED_GLYPH_BATCH],
    source: Range<usize>,
    count: usize,
    pen_x: i64,
    pen_y: i64,
    first_cluster: Option<usize>,
    last_cluster: usize,
    flags_or: u32,
    missing: usize,
}

impl PreparedBatch {
    fn new(request: &NativeMarkdownPagedGlyphPaintRequest<'_>, source: Range<usize>) -> Self {
        Self {
            glyphs: [0; NATIVE_MARKDOWN_PAGED_GLYPH_BATCH],
            positions: [CGPoint { x: 0.0, y: 0.0 }; NATIVE_MARKDOWN_PAGED_GLYPH_BATCH],
            source,
            count: 0,
            pen_x: request.origin.pen_x_26_6,
            pen_y: request.origin.pen_y_26_6,
            first_cluster: None,
            last_cluster: 0,
            flags_or: 0,
            missing: 0,
        }
    }

    fn push(
        &mut self,
        request: &NativeMarkdownPagedGlyphPaintRequest<'_>,
        glyph: MarkdownShapedGlyph,
    ) -> Result<()> {
        // 即使来源已核范围，也必须把实际字形编号与同一NativeFont的真实face上限连接。
        if self.count == NATIVE_MARKDOWN_PAGED_GLYPH_BATCH
            || !self.source.contains(&glyph.source_utf16_cluster)
            || glyph.glyph_id >= request.native_font.core_font().metadata().glyph_count
        {
            return Err(NativeMarkdownPagedGlyphPaintCause::InvalidGlyph);
        }
        self.glyphs[self.count] = u16::try_from(glyph.glyph_id)
            .map_err(|_| NativeMarkdownPagedGlyphPaintCause::InvalidGlyph)?;
        self.positions[self.count] = CGPoint {
            x: axis_position(request.origin.baseline.x, self.pen_x, glyph.x_offset)?,
            y: axis_position(request.origin.baseline.y, self.pen_y, glyph.y_offset)?,
        };
        self.pen_x = advance(self.pen_x, glyph.x_advance)?;
        self.pen_y = advance(self.pen_y, glyph.y_advance)?;
        self.first_cluster.get_or_insert(glyph.source_utf16_cluster);
        self.last_cluster = glyph.source_utf16_cluster;
        self.flags_or |= glyph.flags; // 保留unsafe-break等原事实；绘制不签断行许可。
        self.missing += usize::from(glyph.glyph_id == 0); // .notdef也实际绘制，不能静默抛字。
        self.count += 1;
        Ok(())
    }

    fn draw(
        mut self,
        request: &NativeMarkdownPagedGlyphPaintRequest<'_>,
        context: &CGContext,
        check: &mut impl FnMut() -> Result<()>,
        native_draw_returned: &mut bool,
    ) -> Result<NativeMarkdownPagedGlyphPaintReceipt> {
        let first_source_cluster = self
            .first_cluster
            .ok_or(NativeMarkdownPagedGlyphPaintCause::InvalidRange)?;
        let text_matrix = native_checked(check, || CGContext::text_matrix(Some(context)))?;
        let text_position = native_checked(check, || CGContext::text_position(Some(context)))?;
        check()?;
        CGContext::save_g_state(Some(context));
        let restore = ContextRestore {
            context,
            text_matrix,
            text_position,
        };
        check()?;
        // SAFETY: 两个固定数组的count项均已预检，count非零且不超过256；原CTFont/context
        // 在同步调用内存活，原来源门在前后核验，没有Doc/RefCell租约或部分批次发布。
        unsafe {
            request.native_font.native_font().draw_glyphs(
                NonNull::from(&mut self.glyphs[0]),
                NonNull::from(&mut self.positions[0]),
                self.count,
                context,
            );
        }
        *native_draw_returned = true;
        check()?;
        drop(restore);
        check()?;
        Ok(NativeMarkdownPagedGlyphPaintReceipt {
            glyph_range: request.glyph_range.clone(),
            source_utf16_range: self.source,
            first_source_cluster,
            last_source_cluster: self.last_cluster,
            flags_or: self.flags_or,
            missing_glyphs: self.missing,
            final_pen_x_26_6: self.pen_x,
            final_pen_y_26_6: self.pen_y,
        })
    }
}

fn batch_count(range: &Range<usize>) -> Result<usize> {
    range
        .end
        .checked_sub(range.start)
        .filter(|count| (1..=NATIVE_MARKDOWN_PAGED_GLYPH_BATCH).contains(count))
        .ok_or(NativeMarkdownPagedGlyphPaintCause::BatchLimit)
}

fn native_checked<T>(
    check: &mut impl FnMut() -> Result<()>,
    operation: impl FnOnce() -> T,
) -> Result<T> {
    check()?;
    let value = operation();
    check()?;
    Ok(value)
}

fn validate_native_font(
    request: &NativeMarkdownPagedGlyphPaintRequest<'_>,
    check: &mut impl FnMut() -> Result<()>,
) -> Result<()> {
    // SAFETY: 实际主线程NativeFont覆盖getter生命周期；这里只读原不可变CTFont的公开matrix。
    let matrix = native_checked(check, || unsafe {
        request.native_font.native_font().matrix()
    })?;
    if matrix
        != (CGAffineTransform {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            tx: 0.0,
            ty: 0.0,
        })
    {
        return Err(NativeMarkdownPagedGlyphPaintCause::FontMatrixUnsupported);
    }
    Ok(())
}

/// 将同一Ready任务的指定批次送入真实CoreText；调用者负责context、clip及原回调生命周期。
///
/// 原点沿当前context的y-up用户坐标解释，offset/advance原样按26.6除64；RTL也不得倒排。
/// 调用者必须在进入本函数前结束Doc/RefCell借用，并在Err时丢弃候选像素。超量range整批拒绝，
/// 不截断成256；本函数不申请图形surface、不执行layout、不提供任何软行/选择几何Ready权限。
///
/// # Errors
/// 非Ready、原Source/Font/前台失权、范围/坐标无效或IO失败均拒绝；实际draw后失权显式带返回事实。
pub fn paint_paged_glyphs(
    job: &mut MarkdownShapingJob,
    request: NativeMarkdownPagedGlyphPaintRequest<'_>,
    context: &CGContext,
    current: &(impl Fn() -> bool + ?Sized),
) -> std::result::Result<NativeMarkdownPagedGlyphPaintReceipt, NativeMarkdownPagedGlyphPaintError> {
    let mut native_draw_returned = false;
    let result = (|| -> Result<NativeMarkdownPagedGlyphPaintReceipt> {
        batch_count(&request.glyph_range)?;
        if !current() {
            return Err(NativeMarkdownPagedGlyphPaintCause::Revoked);
        }
        if job.status() != MarkdownShapingStatus::Ready {
            return Err(NativeMarkdownPagedGlyphPaintCause::Pending);
        }
        let profile = job.profile()?;
        if request.glyph_range.end > profile.glyph_count {
            return Err(NativeMarkdownPagedGlyphPaintCause::InvalidRange);
        }
        if !request.native_font.accepts_shaping_size(job.points()) {
            return Err(NativeMarkdownPagedGlyphPaintCause::FontSizeMismatch);
        }
        let proof = Proof {
            native_font_authority: request.native_font.core_font().authority(),
            request: &request,
            glyph_count: profile.glyph_count,
        };
        proof.current(job, current)?;
        validate_native_font(&request, &mut || proof.current(job, current))?;
        let source = job.source_range();
        let mut batch = PreparedBatch::new(&request, source);
        for index in request.glyph_range.clone() {
            // 固定批次全部准备完才进行唯一draw；IO/任何坐标拒绝都不会留下半批绘图。
            proof.current(job, current)?;
            let glyph = job
                .glyph(request.source, request.font_authority, index)?
                .ok_or(NativeMarkdownPagedGlyphPaintCause::InvalidRange)?;
            batch.push(&request, glyph)?;
        }
        batch.draw(
            &request,
            context,
            &mut || proof.current(job, current),
            &mut native_draw_returned,
        )
    })();
    result.map_err(|cause| NativeMarkdownPagedGlyphPaintError {
        cause,
        native_draw_returned,
    })
}
