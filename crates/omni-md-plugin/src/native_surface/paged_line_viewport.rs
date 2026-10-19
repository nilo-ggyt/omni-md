//! 已封存真实行的有限窗口；只消费原Line/Part、字体和Scene票，不解析或整形正文。

use std::{cell::RefCell, ops::Range, rc::Rc};

use objc2_core_foundation::CGPoint;
use objc2_foundation::{NSPoint, NSRange, NSRect, NSSize};
use omni_markdown_core::{
    MARKDOWN_LINE_FONT_SLOTS, MarkdownComposedLine, MarkdownComposedLinePart,
    MarkdownComposedLines, MarkdownLineComposeError,
};

use super::{
    paged_line_fonts::{NativeMarkdownLineFont, NativeMarkdownLineFontError},
    paged_line_geometry::NativeMarkdownLineGeometryError,
    paged_text_view::{NativeMarkdownPagedCharacterRect, NativeMarkdownPagedSceneKey},
};

mod caret;
mod geometry;
mod paint;
mod visual;
pub use caret::NativeMarkdownComposedCaretGeometry;
pub use geometry::NativeMarkdownComposedViewportHit;
pub use paint::{
    NativeMarkdownComposedViewportPaintBudget, NativeMarkdownComposedViewportPaintCause,
    NativeMarkdownComposedViewportPaintCompletion, NativeMarkdownComposedViewportPaintError,
    NativeMarkdownComposedViewportPaintOutcome, NativeMarkdownComposedViewportPaintReceipt,
    NativeMarkdownComposedViewportPaintSession, NativeMarkdownComposedViewportPaintStep,
    NativeMarkdownComposedViewportPaintWork,
};
pub use visual::NativeMarkdownComposedVisualTarget;

/// 限定一次已准备窗口的元数据；更大窗口由原producer分批准备，不能截断后宣称完整。
pub const NATIVE_MARKDOWN_COMPOSED_VIEWPORT_LINES: usize = 256;
pub const NATIVE_MARKDOWN_COMPOSED_VIEWPORT_PARTS: usize = 1_024;
/// 只约束旧同步paint；分步会话按每步配额推进，不以这个总量截断可见字形。
pub const NATIVE_MARKDOWN_COMPOSED_VIEWPORT_GLYPHS: usize = 4_096;
const MAX_POINT: f64 = (1_u64 << 46) as f64;

#[derive(Debug, thiserror::Error)]
pub enum NativeMarkdownComposedViewportError {
    #[error("原可见行窗口尚未准备此位置或超出本次固定配额")]
    Pending,
    #[error("原可见行窗口权限已经撤销")]
    Revoked,
    #[error("原行输出正在被使用，不能重入读取")]
    Busy,
    #[error("原窗口的实际几何无效或溢出")]
    InvalidGeometry,
    #[error("原窗口行不连续或不属于同一Source")]
    InvalidCoverage,
    #[error("原范围无效或切入尚不支持的文字边界")]
    InvalidRange,
    #[error("原palette没有这个part实际使用的字体")]
    FontMissing,
    #[error("原行目录读取失败：{0}")]
    Composed(#[from] MarkdownLineComposeError),
    #[error("原字体注册或度量失效：{0}")]
    Font(#[from] NativeMarkdownLineFontError),
    #[error("原行文字几何失效：{0}")]
    Geometry(#[from] NativeMarkdownLineGeometryError),
}
type Result<T> = std::result::Result<T, NativeMarkdownComposedViewportError>;

struct Part {
    handle: MarkdownComposedLinePart,
    font: usize,
}
struct Line {
    handle: MarkdownComposedLine,
    parts: Vec<Part>,
    frame: NSRect,
    baseline: CGPoint,
    ascent: f64,
    descent: f64,
}

/// 强边只覆盖同一个封存Reader、有限行元数据和有限palette；没有正文、glyph或GCB列表。
pub struct NativeMarkdownComposedViewport {
    output: Rc<RefCell<MarkdownComposedLines>>,
    key: NativeMarkdownPagedSceneKey,
    palette: Box<[Rc<NativeMarkdownLineFont>]>,
    lines: Vec<Line>,
    glyphs: usize,
}

impl NativeMarkdownComposedViewport {
    /// 在原coalesced准备阶段安装有限连续Line；首top只能来自原virtual-scroll生产者的真实锚点。
    ///
    /// 坐标使用实际正文的y-up空间，后续行仅从同palette真实ascent/descent/leading计算。
    /// 本组件尚未合入段落间距、缩进和lineHeightMultiple，不能作为完整预览对等证据。
    ///
    /// # Errors
    /// 行/part元数据超预算、未封存/失效输出、外来handle、缺失字体和无效几何均拒绝整个候选。
    pub fn prepare(
        output: Rc<RefCell<MarkdownComposedLines>>,
        key: NativeMarkdownPagedSceneKey,
        palette: Vec<Rc<NativeMarkdownLineFont>>,
        handles: impl IntoIterator<Item = MarkdownComposedLine>,
        first_top: NSPoint,
        current: &dyn Fn() -> bool,
    ) -> Result<Self> {
        require_output(&output, &key, current)?;
        // palette只包含当前真实Native字体；空表或超槽表不能靠名字重建旧part的字体。
        if palette.is_empty() || palette.len() > MARKDOWN_LINE_FONT_SLOTS {
            return Err(NativeMarkdownComposedViewportError::FontMissing);
        }
        // 原Scene几何与虚拟滚动来源都必须有限；不把NaN/巨大坐标夹回一个貌似正常的视口。
        if !point(first_top)
            || !rect(key.request.viewport)
            || !rect(key.request.frame_in_parent)
            || !key.request.bounds_width.is_finite()
            || key.request.bounds_width <= 0.0
            || key.request.bounds_width >= MAX_POINT
            || !point(key.request.virtual_viewport.document_origin)
        {
            return Err(NativeMarkdownComposedViewportError::InvalidGeometry);
        }
        let palette = palette.into_boxed_slice(); // 不保留调用者可能传入的超额Vec容量。
        let mut lines: Vec<Line> = Vec::new();
        let mut top = first_top.y;
        let mut parts = 0usize;
        let mut glyphs = 0usize;
        // 只接收本次有限窗口；预算不足时整个候选归还，producer可另行准备下一批。
        for handle in handles {
            if lines.len() == NATIVE_MARKDOWN_COMPOSED_VIEWPORT_LINES
                || handle.facts().parts
                    > NATIVE_MARKDOWN_COMPOSED_VIEWPORT_PARTS.saturating_sub(parts)
            {
                return Err(NativeMarkdownComposedViewportError::Pending);
            }
            if handle.facts().source_utf16.end > key.request.input.utf16_len {
                return Err(NativeMarkdownComposedViewportError::InvalidCoverage);
            }
            // 连续handle同时要求真实源坐标与真实行ordinal相邻，不能拼接有空洞的缓存行。
            if let Some(previous) = lines.last()
                && (previous.handle.facts().source_utf16.end != handle.facts().source_utf16.start
                    || previous.handle.facts().ordinal.checked_add(1)
                        != Some(handle.facts().ordinal))
            {
                return Err(NativeMarkdownComposedViewportError::InvalidCoverage);
            }
            let mut prepared = Vec::with_capacity(handle.facts().parts);
            let glyphs_before = glyphs;
            let (mut ascent, mut descent, mut leading) = (0.0_f64, 0.0_f64, f64::NEG_INFINITY);
            // 每个part只有弱handle和font索引；不把glyph、GCB或源文字装进这个Vec。
            for ordinal in 0..handle.facts().parts {
                // 只在纯Core读取期借Reader；字体/Native和可重入current全部在借用结束后运行。
                let part = {
                    let mut reader = output
                        .try_borrow_mut()
                        .map_err(|_| NativeMarkdownComposedViewportError::Busy)?;
                    reader.require(&key.source)?;
                    reader
                        .part(&handle, ordinal)?
                        .ok_or(NativeMarkdownComposedViewportError::InvalidCoverage)?
                };
                require_output(&output, &key, current)?;
                // 原注册票定位同一个Font payload，名称/字号相同的另一次Native capture不能冒充。
                let authority = part.font_registration().authority()?;
                let font = palette
                    .iter()
                    .position(|font| font.native().core_font().permits(&authority))
                    .ok_or(NativeMarkdownComposedViewportError::FontMissing)?;
                palette[font].require(part.font_registration())?;
                let metrics = palette[font].metrics()?;
                ascent = ascent.max(metrics.ascent);
                descent = descent.max(metrics.descent);
                leading = leading.max(metrics.leading); // 保留真实leading符号，不补固定行高或按ordinal推测全局y。
                glyphs = glyphs
                    .checked_add(part.facts().glyphs)
                    .ok_or(NativeMarkdownComposedViewportError::InvalidGeometry)?;
                prepared.push(Part { handle: part, font });
            }
            if glyphs.checked_sub(glyphs_before) != Some(handle.facts().glyphs) {
                return Err(NativeMarkdownComposedViewportError::InvalidCoverage);
            }
            // 同行字体共用实际baseline；leading分在上下，下一行top只从真实度量累计。
            let height = ascent + descent + leading;
            let baseline = CGPoint {
                x: first_top.x,
                y: top - ascent - leading / 2.0,
            };
            let frame = NSRect::new(
                NSPoint::new(key.request.viewport.origin.x, top - height),
                NSSize::new(key.request.viewport.size.width, height),
            );
            if !point(baseline) || !rect(frame) {
                return Err(NativeMarkdownComposedViewportError::InvalidGeometry);
            } // 空字体行或非有限高度不能产生占位行框。
            parts += prepared.len();
            top -= height;
            lines.push(Line {
                handle,
                parts: prepared,
                frame,
                baseline,
                ascent,
                descent,
            });
        }
        if lines.is_empty() {
            return Err(NativeMarkdownComposedViewportError::Pending);
        } // 没有真实行的窗口不签Ready；空文档仍须真实控制part/字体生产者。
        let candidate = Self {
            output,
            key,
            palette,
            lines,
            glyphs,
        };
        candidate.require(current)?; // 发布前再次核原Source和scene nonce，不把部分metadata当成功。
        Ok(candidate)
    }

    #[must_use]
    pub fn key(&self) -> &NativeMarkdownPagedSceneKey {
        &self.key
    }

    /// 只返回已准备窗口的范围；不把已封存前缀或末个可见行当作全文结尾。
    #[must_use]
    pub fn source_range(&self) -> Range<usize> {
        self.lines[0].handle.facts().source_utf16.start
            ..self.lines[self.lines.len() - 1]
                .handle
                .facts()
                .source_utf16
                .end
    }

    #[must_use]
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// 返回真实metrics组成的本地行框，供原producer维护其可验证virtual-scroll锚点。
    #[must_use]
    pub fn line_frame(&self, ordinal_in_window: usize) -> Option<NSRect> {
        self.lines.get(ordinal_in_window).map(|line| line.frame)
    }

    /// 复用已发布像素前核原输出、全部有限part和真实字体，不重新排版或读取正文。
    ///
    /// 此门不能由Scene nonce或bool current替代：Source首错、Reader关闭/失效、旧字体
    /// 注册和实际字号不符都必须撤销旧像素复用。零glyph批次仍沿原Reader健康和part
    /// 身份合同检查，不构造字符串、字形副本或新的Source捕获。
    ///
    /// # Errors
    /// 原Scene/Source/Reader/Font失效或借用冲突时拒绝；真实Source错误优先于布尔撤权。
    pub fn validate_ready(&self, current: &dyn Fn() -> bool) -> Result<()> {
        self.require(current)?;
        for line in &self.lines {
            for part in &line.parts {
                let validation = (|| -> Result<()> {
                    let font = &self.palette[part.font];
                    font.require(part.handle.font_registration())?;
                    if !font
                        .native()
                        .accepts_shaping_size(part.handle.facts().points)
                    {
                        return Err(NativeMarkdownLineGeometryError::FontSizeMismatch.into());
                    } // 相同注册名称或近似字号不能给原封存输出补签另一个Native字体。
                    font.metrics()?; // 度量本身仍须绑定原Font authority，不能用纯数值缓存续权。
                    let mut reader = self
                        .output
                        .try_borrow_mut()
                        .map_err(|_| NativeMarkdownComposedViewportError::Busy)?;
                    reader.require(&self.key.source)?;
                    reader.read_batch(&part.handle, 0..0, &mut [])?;
                    Ok(())
                })();
                self.require(current)?; // Reader和注册临时租约均已归还，再允许current重入；Source首错优先。
                validation?;
            }
        }
        self.require(current)
    }

    fn require(&self, current: &dyn Fn() -> bool) -> Result<()> {
        require_output(&self.output, &self.key, current)
    }

    fn visible(&self, frame: NSRect) -> bool {
        intersects(frame, self.key.request.viewport)
    }
}

/// 原Source故障必须压过bool撤权；current两边都归还Reader再调用，不让重入掩盖EIO。
fn require_output(
    output: &RefCell<MarkdownComposedLines>,
    key: &NativeMarkdownPagedSceneKey,
    current: &dyn Fn() -> bool,
) -> Result<()> {
    let before = current();
    {
        let reader = output
            .try_borrow()
            .map_err(|_| NativeMarkdownComposedViewportError::Busy)?;
        reader.require(&key.source)?;
    }
    let after = current(); // 首次检查可能重入；下面仍读原Source，不通过新capture恢复权限。
    {
        let reader = output
            .try_borrow()
            .map_err(|_| NativeMarkdownComposedViewportError::Busy)?;
        reader.require(&key.source)?;
    }
    if !before || !after || !key.nonce.same(&key.nonce) {
        return Err(NativeMarkdownComposedViewportError::Revoked);
    }
    Ok(())
}

fn point(value: CGPoint) -> bool {
    value.x.is_finite()
        && value.y.is_finite()
        && value.x.abs() < MAX_POINT
        && value.y.abs() < MAX_POINT
}
fn rect(value: NSRect) -> bool {
    point(value.origin)
        && value.size.width.is_finite()
        && value.size.height.is_finite()
        && value.size.width > 0.0
        && value.size.height > 0.0
        && point(NSPoint::new(
            value.origin.x + value.size.width,
            value.origin.y + value.size.height,
        ))
}
fn intersects(first: NSRect, second: NSRect) -> bool {
    first.origin.x < second.origin.x + second.size.width
        && second.origin.x < first.origin.x + first.size.width
        && first.origin.y < second.origin.y + second.size.height
        && second.origin.y < first.origin.y + first.size.height
}
