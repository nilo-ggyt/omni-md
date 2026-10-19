//! 一个未发布位图上的可消费游标；每步只读有限原glyph，不以同步总量门禁截掉正文。

use super::*;
use objc2_core_foundation::{CFRetained, CGAffineTransform};
use objc2_core_graphics::{
    CGBitmapContextGetBitsPerPixel, CGBitmapContextGetBytesPerRow, CGBitmapContextGetData,
    CGBitmapContextGetHeight, CGBitmapContextGetWidth,
};
use objc2_foundation::MainThreadMarker;
use std::{fmt, ptr};

const METADATA_LIMIT: usize = 32;

/// 配额只限制本步；零配额保留原游标，不重做已经返回的Native draw。
#[derive(Clone, Copy, Debug)]
pub struct NativeMarkdownComposedViewportPaintBudget {
    pub glyphs: usize,
    pub metadata_records: usize,
}
impl Default for NativeMarkdownComposedViewportPaintBudget {
    fn default() -> Self {
        Self {
            glyphs: NATIVE_MARKDOWN_PAGED_GLYPH_BATCH,
            metadata_records: METADATA_LIMIT,
        }
    }
}

/// 元数据记录计入真实行进入、part进入和advance核验；不计成正文输入字节。
#[derive(Clone, Copy, Debug, Default)]
pub struct NativeMarkdownComposedViewportPaintWork {
    pub glyphs: usize,
    pub batches: usize,
    pub metadata_records: usize,
}

#[derive(Debug)]
pub enum NativeMarkdownComposedViewportPaintOutcome {
    Pending(NativeMarkdownComposedViewportPaintSession),
    Complete(NativeMarkdownComposedViewportPaintCompletion),
}
#[derive(Debug)]
pub struct NativeMarkdownComposedViewportPaintStep {
    pub work: NativeMarkdownComposedViewportPaintWork,
    pub outcome: NativeMarkdownComposedViewportPaintOutcome,
}

/// 只有真实游标走完才签发；弱Scene身份外显式保有原context claim，防止地址回收后ABA冒签。
/// 消费者须归还本收据及自己的最后context，核真实释放callback后才允许封存像素。
pub struct NativeMarkdownComposedViewportPaintCompletion {
    key: NativeMarkdownPagedSceneKey,
    context: CFRetained<CGContext>,
    receipt: NativeMarkdownComposedViewportPaintReceipt,
    work: NativeMarkdownComposedViewportPaintWork,
}
impl NativeMarkdownComposedViewportPaintCompletion {
    /// 只核同一次候选身份；使用者仍须复核原Scene健康、几何和实际context释放后才能发布。
    #[must_use]
    pub fn permits(&self, key: &NativeMarkdownPagedSceneKey, context: &CGContext) -> bool {
        self.key.same(key) && ptr::eq(&*self.context, context)
    }
    /// 桥接者只转交同一个实际claim；额外保留会阻止真实像素seal，不能作为无资源收据缓存。
    #[must_use]
    pub fn context_claim(&self) -> CFRetained<CGContext> {
        self.context.clone()
    }
    #[must_use]
    pub fn receipt(&self) -> NativeMarkdownComposedViewportPaintReceipt {
        self.receipt
    }
    #[must_use]
    pub fn work(&self) -> NativeMarkdownComposedViewportPaintWork {
        self.work
    }
}
impl fmt::Debug for NativeMarkdownComposedViewportPaintCompletion {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("NativeMarkdownComposedViewportPaintCompletion")
            .field("context", &ptr::from_ref(&*self.context))
            .field("receipt", &self.receipt)
            .field("work", &self.work)
            .finish_non_exhaustive()
    }
}

/// 各状态只保原窗口下标和真实pen；不能用新Line/Part或按字符估算的advance补签。
#[derive(Clone, Copy, Debug)]
enum Cursor {
    Line(usize),
    Part {
        line: usize,
        part: usize,
    },
    Glyphs {
        line: usize,
        part: usize,
        start: usize,
        pen_x: i64,
        pen_y: i64,
    },
    Finish {
        line: usize,
        part: usize,
        pen_x: i64,
        pen_y: i64,
    },
}

#[derive(Clone, Copy, PartialEq)]
struct ContextGeometry {
    transform: CGAffineTransform,
    clip: NSRect,
}
impl ContextGeometry {
    fn read(
        context: &CGContext,
    ) -> std::result::Result<Self, NativeMarkdownComposedViewportPaintCause> {
        let width = CGBitmapContextGetWidth(Some(context));
        let height = CGBitmapContextGetHeight(Some(context));
        let row = CGBitmapContextGetBytesPerRow(Some(context));
        // 这里只接受原私有RGBA位图，拒绝窗口/PDF显示列表；原分配寿命由context的真实回调保护。
        if CGBitmapContextGetData(Some(context)).is_null()
            || CGBitmapContextGetBitsPerPixel(Some(context)) != 32
            || width == 0
            || height == 0
            || width.checked_mul(4).is_none_or(|bytes| bytes > row)
            || row
                .checked_mul(height)
                .is_none_or(|bytes| bytes > 32 * 1024 * 1024)
        {
            return Err(NativeMarkdownComposedViewportPaintCause::ContextChanged);
        }
        let transform = CGContext::ctm(Some(context));
        let clip = CGContext::clip_bounding_box(Some(context));
        let determinant = transform.a * transform.d - transform.b * transform.c;
        if ![
            transform.a,
            transform.b,
            transform.c,
            transform.d,
            transform.tx,
            transform.ty,
        ]
        .into_iter()
        .all(f64::is_finite)
            || !determinant.is_finite()
            || determinant == 0.0
            || !point(clip.origin)
            || !clip.size.width.is_finite()
            || !clip.size.height.is_finite()
            || clip.size.width < 0.0
            || clip.size.height < 0.0
            || !point(NSPoint::new(
                clip.origin.x + clip.size.width,
                clip.origin.y + clip.size.height,
            ))
        {
            return Err(NativeMarkdownComposedViewportPaintCause::ContextChanged);
        } // 无效矩阵/clip不能被夹回原geometry，随后各步必须与本次原读值精确相同。
        Ok(Self { transform, clip })
    }
}

/// 未完成候选持原有限窗口和原context；取消/失败真实释放，完成只把context转交线性完成票。
///
/// 调用者须独占未发布context的写入和样式状态，不能让其他事件改其颜色或图形栈。
/// 本层额外核原context、CTM和clip，任何失配都使整幅候选作废；已完成tile不能提前显示。
pub struct NativeMarkdownComposedViewportPaintSession {
    window: Rc<NativeMarkdownComposedViewport>,
    context: CFRetained<CGContext>,
    geometry: ContextGeometry,
    cursor: Cursor,
    receipt: NativeMarkdownComposedViewportPaintReceipt,
    total: NativeMarkdownComposedViewportPaintWork,
    native_draw_returned: bool,
}
impl fmt::Debug for NativeMarkdownComposedViewportPaintSession {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 诊断只打印标量；Debug不能触发原生getter、Reader读取或正文物化。
        out.debug_struct("NativeMarkdownComposedViewportPaintSession")
            .field("cursor", &self.cursor)
            .field("receipt", &self.receipt)
            .field("total", &self.total)
            .finish_non_exhaustive()
    }
}

impl NativeMarkdownComposedViewport {
    /// 领取同一未发布CPU候选的原游标，不受4096同步glyph总量限制，也不提前画前缀。
    ///
    /// # Errors
    /// 原Source/Scene失权、非主线程或非有限私有RGBA位图均拒绝，并归还传入claim。
    pub fn begin_paint(
        self: Rc<Self>,
        context: CFRetained<CGContext>,
        current: &dyn Fn() -> bool,
    ) -> std::result::Result<
        NativeMarkdownComposedViewportPaintSession,
        NativeMarkdownComposedViewportPaintError,
    > {
        let result = (|| {
            self.require(current)?;
            if MainThreadMarker::new().is_none() {
                return Err(NativeMarkdownComposedViewportPaintCause::WrongThread);
            }
            let geometry = ContextGeometry::read(&context);
            self.require(current)?; // 原Source首错优先于context检查；current可能同步撤销原Reader。
            geometry
        })();
        let geometry = result.map_err(|cause| NativeMarkdownComposedViewportPaintError {
            cause,
            native_draw_returned: false,
        })?;
        Ok(NativeMarkdownComposedViewportPaintSession {
            window: self,
            context,
            geometry,
            cursor: Cursor::Line(0),
            receipt: NativeMarkdownComposedViewportPaintReceipt {
                glyphs: 0,
                batches: 0,
                missing_glyphs: 0,
            },
            total: NativeMarkdownComposedViewportPaintWork::default(),
            native_draw_returned: false,
        })
    }
}

impl NativeMarkdownComposedViewportPaintSession {
    fn require(
        &self,
        current: &dyn Fn() -> bool,
    ) -> std::result::Result<(), NativeMarkdownComposedViewportPaintCause> {
        self.window.require(current)?;
        if MainThreadMarker::new().is_none() {
            return Err(NativeMarkdownComposedViewportPaintCause::WrongThread);
        }
        let actual = ContextGeometry::read(&self.context);
        self.window.require(current)?; // 所有Reader租约已归还再进Native/current，真实Source失败先返回。
        if actual? != self.geometry {
            return Err(NativeMarkdownComposedViewportPaintCause::ContextChanged);
        }
        Ok(())
    }

    /// 消费原状态后只归还一个Pending或完成收据；Err不会留下能重放已画前缀的会话。
    ///
    /// # Errors
    /// 配额越界、原票/位图改变、IO或真实advance不符均丢弃候选；错误保留此前draw事实。
    pub fn step(
        mut self,
        budget: NativeMarkdownComposedViewportPaintBudget,
        current: &dyn Fn() -> bool,
    ) -> std::result::Result<
        NativeMarkdownComposedViewportPaintStep,
        NativeMarkdownComposedViewportPaintError,
    > {
        let mut work = NativeMarkdownComposedViewportPaintWork::default();
        let result = self.advance(budget, &mut work, current);
        match result {
            Ok(false) => Ok(NativeMarkdownComposedViewportPaintStep {
                work,
                outcome: NativeMarkdownComposedViewportPaintOutcome::Pending(self),
            }), // 只有原游标继续；不再捕获Source、创建新的Scene或重复已经画过的批次。
            Ok(true) => {
                let completion = NativeMarkdownComposedViewportPaintCompletion {
                    key: self.window.key.clone(),
                    context: self.context.clone(),
                    receipt: self.receipt,
                    work: self.total,
                };
                drop(self); // 归还窗口等业务强边；context仍由完成票和WritableTile显式持有，不能提前签释放。
                Ok(NativeMarkdownComposedViewportPaintStep {
                    work,
                    outcome: NativeMarkdownComposedViewportPaintOutcome::Complete(completion),
                })
            }
            Err(cause) => Err(NativeMarkdownComposedViewportPaintError {
                cause,
                native_draw_returned: self.native_draw_returned,
            }), // self随此失败退出真实析构；后续取消不能覆盖首个IO错误或再发布部分像素。
        }
    }

    fn advance(
        &mut self,
        budget: NativeMarkdownComposedViewportPaintBudget,
        work: &mut NativeMarkdownComposedViewportPaintWork,
        current: &dyn Fn() -> bool,
    ) -> std::result::Result<bool, NativeMarkdownComposedViewportPaintCause> {
        self.require(current)?;
        if budget.glyphs > NATIVE_MARKDOWN_PAGED_GLYPH_BATCH
            || budget.metadata_records > METADATA_LIMIT
        {
            return Err(NativeMarkdownComposedViewportPaintCause::InvalidBudget);
        }
        loop {
            self.require(current)?;
            match self.cursor {
                Cursor::Line(line) => {
                    if line == self.window.lines.len() {
                        return Ok(true); // 所有实际行和part都走完；没有以总glyph上限截掉尾部。
                    }
                    if !self.take_metadata(budget, work)? {
                        return Ok(false);
                    }
                    self.cursor = if self.window.visible(self.window.lines[line].frame) {
                        Cursor::Part { line, part: 0 }
                    } else {
                        Cursor::Line(line + 1)
                    }; // 同步oracle的原viewport可见性不变；不以tile框猜glyph墨迹界而漏掉组合记号。
                }
                Cursor::Part { line, part } => {
                    if !self.take_metadata(budget, work)? {
                        return Ok(false);
                    }
                    let Some(original) = self.window.lines[line].parts.get(part) else {
                        self.cursor = Cursor::Line(line + 1);
                        continue; // 只越过已准备的真实末part，不从缺页推断全文EOF。
                    };
                    let facts = original.handle.facts();
                    self.cursor = if facts.kind == MarkdownComposedLinePartKind::Glyphs {
                        Cursor::Glyphs {
                            line,
                            part,
                            start: 0,
                            pen_x: facts.origin_x_26_6,
                            pen_y: facts.origin_y_26_6,
                        }
                    } else {
                        Cursor::Part {
                            line,
                            part: part + 1,
                        }
                    }; // Tab/分隔符保留真实布局贡献，绝不补.notdef或伪造一个字形。
                }
                Cursor::Glyphs {
                    line,
                    part,
                    start,
                    pen_x,
                    pen_y,
                } => {
                    let original = &self.window.lines[line].parts[part];
                    let facts = original.handle.facts();
                    if start == facts.glyphs {
                        self.cursor = Cursor::Finish {
                            line,
                            part,
                            pen_x,
                            pen_y,
                        };
                        continue; // 空glyph part也要走最终advance核验；不能因未draw直接跳过。
                    }
                    if work.glyphs == budget.glyphs {
                        return Ok(false); // 原part内精确续接；不能把part前缀解释为整个tile完成。
                    }
                    let end = start
                        .saturating_add(budget.glyphs - work.glyphs)
                        .min(facts.glyphs);
                    let font = &self.window.palette[original.font];
                    let authority = font.native().core_font().authority();
                    CGContext::save_g_state(Some(&self.context));
                    let restore = Restore(&self.context);
                    CGContext::clip_to_rect(Some(&self.context), self.window.key.request.viewport);
                    let painted = paint_composed_glyphs(
                        &self.window.output,
                        &original.handle,
                        NativeMarkdownPagedGlyphPaintRequest {
                            source: &self.window.key.source,
                            font_authority: &authority,
                            native_font: font.native(),
                            glyph_range: start..end,
                            origin: NativeMarkdownPagedGlyphOrigin {
                                baseline: self.window.lines[line].baseline,
                                pen_x_26_6: pen_x,
                                pen_y_26_6: pen_y,
                            },
                        },
                        &self.context,
                        &|| self.window.require(current).is_ok(),
                    );
                    self.native_draw_returned |= painted
                        .as_ref()
                        .map_or_else(|error| error.native_draw_returned, |_| true);
                    drop(restore); // 还原本步clip后才检查原geometry；不把临时clip当作新的候选坐标。
                    self.require(current)?;
                    let painted =
                        painted.map_err(NativeMarkdownComposedViewportPaintCause::Glyph)?;
                    work.glyphs += end - start;
                    work.batches += 1;
                    self.receipt.glyphs = checked_add(self.receipt.glyphs, end - start)?;
                    self.receipt.batches = checked_add(self.receipt.batches, 1)?;
                    self.receipt.missing_glyphs =
                        checked_add(self.receipt.missing_glyphs, painted.missing_glyphs)?;
                    self.total.glyphs = self.receipt.glyphs;
                    self.total.batches = self.receipt.batches;
                    self.cursor = Cursor::Glyphs {
                        line,
                        part,
                        start: end,
                        pen_x: painted.final_pen_x_26_6,
                        pen_y: painted.final_pen_y_26_6,
                    }; // pen只取原painter真实receipt，不能按glyph数/平均字宽或下一行origin重建。
                }
                Cursor::Finish {
                    line,
                    part,
                    pen_x,
                    pen_y,
                } => {
                    if !self.take_metadata(budget, work)? {
                        return Ok(false);
                    }
                    let facts = self.window.lines[line].parts[part].handle.facts();
                    if facts.origin_x_26_6.checked_add(facts.x_advance_26_6) != Some(pen_x)
                        || facts.origin_y_26_6.checked_add(facts.y_advance_26_6) != Some(pen_y)
                    {
                        return Err(NativeMarkdownComposedViewportPaintCause::InconsistentAdvance);
                    }
                    self.cursor = Cursor::Part {
                        line,
                        part: part + 1,
                    };
                }
            }
        }
    }

    fn take_metadata(
        &mut self,
        budget: NativeMarkdownComposedViewportPaintBudget,
        work: &mut NativeMarkdownComposedViewportPaintWork,
    ) -> std::result::Result<bool, NativeMarkdownComposedViewportPaintCause> {
        if work.metadata_records == budget.metadata_records {
            return Ok(false); // 元数据也在原游标处暂停，避免大量零glyph控制项构成无界同步循环。
        }
        work.metadata_records += 1;
        self.total.metadata_records = checked_add(self.total.metadata_records, 1)?;
        Ok(true)
    }
}

fn checked_add(
    left: usize,
    right: usize,
) -> std::result::Result<usize, NativeMarkdownComposedViewportPaintCause> {
    left.checked_add(right)
        .ok_or(NativeMarkdownComposedViewportPaintCause::CounterExhausted)
}
