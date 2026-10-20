//! 已封存真实行的有限窗口；只消费原Line/Part、字体和Scene票，不解析或整形正文。

use std::{cell::RefCell, ops::Range, rc::Rc};

use objc2_core_foundation::{CFRetained, CGPoint};
use objc2_core_graphics::{CGColorSpace, kCGColorSpaceSRGB};
use objc2_foundation::{NSPoint, NSRange, NSRect, NSSize};
use omni_markdown_core::{
    MARKDOWN_LINE_FONT_SLOTS, MarkdownComposedLine, MarkdownComposedLinePart,
    MarkdownComposedLines, MarkdownLayoutTextAuthority, MarkdownLineComposeError,
    MarkdownLineFontRegistration, MarkdownPresentationMode, MarkdownPresentationStyles,
    MarkdownTableCellDisplaySource,
};

use super::{
    paged_line_fonts::{NativeMarkdownLineFont, NativeMarkdownLineFontError},
    paged_line_geometry::NativeMarkdownLineGeometryError,
    paged_text_view::{NativeMarkdownPagedCharacterRect, NativeMarkdownPagedSceneKey},
};

mod caret;
mod table_cell;
pub use table_cell::NativeMarkdownComposedTableCell;
mod line_alignment;
pub(in crate::native_surface) use line_alignment::NativeMarkdownLineAlignmentMeasure;
pub use line_alignment::NativeMarkdownMeasuredLine;
mod geometry;
mod paint;
mod visual;
pub use caret::NativeMarkdownComposedCaretGeometry;
pub use geometry::NativeMarkdownComposedViewportHit;
pub(in crate::native_surface) use paint::{
    NativeMarkdownComposedViewportDecorationCursor, NativeMarkdownComposedViewportDecorationStep,
};
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
    #[error("无法创建原绘图的命名 sRGB 颜色空间")]
    ColorSpaceUnavailable,
    #[error("原行目录读取失败：{0}")]
    Composed(#[from] MarkdownLineComposeError),
    #[error("原表格显示来源失效：{0:?}")]
    Table(omni_markdown_core::markdown_document_buffer::MarkdownTableRequestError),
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

impl Part {
    /// 代码背景有面积的原精确谓词；准备和实际绘制共用，不由字形墨迹或相邻样式推断。
    fn has_inline_code_background(&self) -> bool {
        let facts = self.handle.facts();
        !facts
            .presentation
            .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
            && facts
                .presentation
                .contains(MarkdownPresentationStyles::INLINE_CODE)
            && facts.x_advance_26_6 != 0
    }
}

/// 每个实际使用的 Font 只保同输出的原弱注册票；同名或另一 catalog 的活 Font 不能代替它续权。
struct FontUse {
    index: usize,
    registration: MarkdownLineFontRegistration,
}

struct Line {
    handle: MarkdownComposedLine,
    parts: Vec<Part>,
    // 与同一封存 parts 共生命周期；false 证明逐 part 背景循环不会产生任何像素。
    // 不保存正文、字形或额外资源，也不代替行块背景与后继 Source/Font 健康门。
    has_inline_code_background: bool,
    frame: NSRect,
    baseline: CGPoint,
    ascent: f64,
    descent: f64,
    paragraph: super::preview_paragraph::PreviewParagraph,
    geometry: super::preview_paragraph::PreviewLineGeometry,
}

/// 强边只覆盖同一个封存Reader、有限行元数据和有限palette；没有正文、glyph或GCB列表。
pub struct NativeMarkdownComposedViewport {
    output: Rc<RefCell<MarkdownComposedLines>>,
    key: NativeMarkdownPagedSceneKey,
    domain: LayoutDomain,
    palette: Box<[Rc<NativeMarkdownLineFont>]>,
    fonts: Box<[FontUse]>,
    lines: Vec<Line>,
    glyphs: usize,
    // 调色板分量属于 sRGB；目标 bitmap 的空间不决定 CGContext 颜色 setter 的输入空间。
    // 当前窗口仅持这一份命名空间，随 Reader/Font 窗口一起归还，不逐 part 或 glyph 建 CGColor。
    paint_space: CFRetained<CGColorSpace>,
}

/// Body 沿原 SceneKey；只读格另持同一真实 display Source，不伪造 Body Input 或本地文档。
enum LayoutDomain {
    Body,
    TableCell {
        display: Rc<MarkdownTableCellDisplaySource>,
        alignment: omni_markdown_core::MarkdownTableAlignment,
        /// 完整实测内容 lane；横向 clip 只裁像素，不改变每条软行的对齐基准。
        width: f64,
        clip: NSRect,
        identity: Rc<()>,
    },
}

impl LayoutDomain {
    /// 活动格从同 session/source 的 TargetDomain 取方向，只读格从原 raw 票取方向。
    /// 普通 Body 保既有段落几何；不把整张表或滚动后的 clip 宽当成活动格行宽。
    fn line_origin_x(
        &self,
        key: &NativeMarkdownPagedSceneKey,
        first_x: f64,
        inset: f64,
        extent: Option<&line_alignment::Extent>,
    ) -> Result<f64> {
        let (alignment, width) = match self {
            Self::Body => {
                let Some(alignment) = key.request.target.cell_alignment() else {
                    return Ok(first_x + inset);
                };
                // 左边距来自同次 Native 转换后的真实 layout 起点；不靠 Source/Preview 常量兜底。
                let layout_left = key.request.virtual_viewport.layout_origin_x
                    - key.request.virtual_viewport.document_origin.x;
                let padding = first_x - layout_left;
                (
                    alignment,
                    key.request.virtual_viewport.layout_width - padding * 2.0,
                )
            }
            Self::TableCell {
                alignment, width, ..
            } => (*alignment, *width),
        };
        Ok(first_x + line_alignment::alignment_origin_x(alignment, width, inset, extent)?)
    }

    /// 同一外层场景下，Composer 仍必须消费各自真正捕获的正文或显示来源。
    fn source(&self, key: &NativeMarkdownPagedSceneKey) -> MarkdownLayoutTextAuthority {
        match self {
            Self::Body => key.source.clone(),
            Self::TableCell { display, .. } => display.authority(),
        }
    }

    /// 显示格的局部坐标只到实际转换后的长度；canonical Input 长度不授予内层范围权限。
    fn utf16_len(&self, key: &NativeMarkdownPagedSceneKey) -> usize {
        match self {
            Self::Body => key.request.input.utf16_len,
            Self::TableCell { display, .. } => display.source().utf16_len(),
        }
    }

    /// 图形裁剪属于同一次真实格框与正文视口的交集，不能扩大成整张表的离屏 backing。
    fn clip(&self, key: &NativeMarkdownPagedSceneKey) -> NSRect {
        match self {
            Self::Body => key.request.viewport,
            Self::TableCell { clip, .. } => *clip,
        }
    }

    /// 先归还 Doc 租约再允许 Native/current 重入；冻结显示快照不延长旧 publication 编辑权限。
    fn require(&self, key: &NativeMarkdownPagedSceneKey) -> Result<()> {
        if let Self::TableCell { display, .. } = self {
            let owner = key
                .request
                .document
                .upgrade()
                .ok_or(NativeMarkdownComposedViewportError::Revoked)?;
            let document = owner
                .try_borrow()
                .map_err(|_| NativeMarkdownComposedViewportError::Busy)?;
            display
                .require_for(&document)
                .map_err(NativeMarkdownComposedViewportError::Table)?;
        }
        Ok(())
    }
}

impl NativeMarkdownComposedViewport {
    /// 字形、代码底色和装饰共用同一part基线；选择与caret继续占真实整行槽，不能随脚本字形越出行框。
    fn part_baseline(&self, line: &Line, part: &Part) -> CGPoint {
        let mut baseline = line.baseline;
        baseline.y += super::text_attributes::native_markdown_baseline_offset(
            part.handle.facts().presentation,
            self.key.presentation_mode == omni_markdown_core::MarkdownPresentationMode::Source,
        )
        .unwrap_or(0.0);
        baseline
    }

    /// 在原coalesced准备阶段安装有限连续Line；首top只能来自原virtual-scroll生产者的真实锚点。
    ///
    /// 坐标使用实际正文的y-up空间；Source保原CT浮点，Preview与Producer共用同Font系统行槽/段落结果。
    ///
    /// # Errors
    /// 行/part元数据超预算、未封存/失效输出、外来handle、缺失字体和无效几何均拒绝整个候选。
    pub fn prepare(
        output: Rc<RefCell<MarkdownComposedLines>>,
        key: NativeMarkdownPagedSceneKey,
        palette: Vec<Rc<NativeMarkdownLineFont>>,
        handles: impl IntoIterator<Item = impl Into<NativeMarkdownMeasuredLine>>,
        first_top: NSPoint,
        current: &dyn Fn() -> bool,
    ) -> Result<Self> {
        Self::prepare_domain(
            output,
            key,
            LayoutDomain::Body,
            palette,
            handles,
            first_top,
            current,
        )
    }

    /// 内层格只由同模块的具体 cell consumer 构造；不把显示局部行暴露给 Body 输入。
    fn prepare_domain(
        output: Rc<RefCell<MarkdownComposedLines>>,
        key: NativeMarkdownPagedSceneKey,
        domain: LayoutDomain,
        palette: Vec<Rc<NativeMarkdownLineFont>>,
        handles: impl IntoIterator<Item = impl Into<NativeMarkdownMeasuredLine>>,
        first_top: NSPoint,
        current: &dyn Fn() -> bool,
    ) -> Result<Self> {
        require_output(&output, &key, &domain, current)?;
        let source = domain.source(&key);
        let extent = domain.utf16_len(&key);
        let clip = domain.clip(&key);
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
        let mut fonts: Vec<FontUse> = Vec::new();
        // 只接收本次有限窗口；预算不足时整个候选归还，producer可另行准备下一批。
        for measured in handles {
            let measured: NativeMarkdownMeasuredLine = measured.into();
            let (handle, alignment_extent) = measured.into_parts();
            if lines.len() == NATIVE_MARKDOWN_COMPOSED_VIEWPORT_LINES
                || handle.facts().parts
                    > NATIVE_MARKDOWN_COMPOSED_VIEWPORT_PARTS.saturating_sub(parts)
            {
                return Err(NativeMarkdownComposedViewportError::Pending);
            }
            if handle.facts().source_utf16.end > extent {
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
            let table_row =
                handle.facts().kind == omni_markdown_core::MarkdownComposedLineKind::TableRow;
            if table_row
                && (key.presentation_mode != MarkdownPresentationMode::InlinePreview
                    || !matches!(domain, LayoutDomain::Body))
            {
                return Err(NativeMarkdownComposedViewportError::InvalidCoverage);
            } // 真实表格块行只存在于正文 flow，不能进入 Source 字面路径或另一个格的局部布局。
            let mut prepared = Vec::with_capacity(handle.facts().parts);
            let mut has_inline_code_background = false;
            let glyphs_before = glyphs;
            let mut metrics =
                super::preview_paragraph::PreviewLineMetrics::new(key.presentation_mode);
            // 每个part只有弱handle和font索引；不把glyph、GCB或源文字装进这个Vec。
            for ordinal in 0..handle.facts().parts {
                // 只在纯Core读取期借Reader；字体/Native和可重入current全部在借用结束后运行。
                let part = {
                    let mut reader = output
                        .try_borrow_mut()
                        .map_err(|_| NativeMarkdownComposedViewportError::Busy)?;
                    reader.require(&source)?;
                    let part = reader
                        .part(&handle, ordinal)?
                        .ok_or(NativeMarkdownComposedViewportError::InvalidCoverage)?;
                    // 原输出/注册映射在封存前认证一次；封存后复用不再对每个 part 重读空字形批次。
                    reader.read_batch(&part, 0..0, &mut [])?;
                    part
                };
                require_output(&output, &key, &domain, current)?;
                // 原注册票定位同一个Font payload，名称/字号相同的另一次Native capture不能冒充。
                let authority = part.font_registration().authority()?;
                let font = palette
                    .iter()
                    .position(|font| font.native().core_font().permits(&authority))
                    .ok_or(NativeMarkdownComposedViewportError::FontMissing)?;
                palette[font].require(part.font_registration())?;
                if !fonts.iter().any(|used| used.index == font) {
                    fonts.push(FontUse {
                        index: font,
                        registration: part.font_registration().clone(),
                    });
                } // 同一个输出 catalog 对活 Core payload 只有一个注册值，原弱票不因 part 数重复持有。
                if table_row {
                    if part.facts().kind
                        != omni_markdown_core::MarkdownComposedLinePartKind::TableRow
                        || part.facts().glyphs != 0
                    {
                        return Err(NativeMarkdownComposedViewportError::InvalidCoverage);
                    }
                } else {
                    if !palette[font]
                        .native()
                        .accepts_shaping_size(part.facts().points)
                    {
                        // 准备阶段保留真实字号错配的数值，不能让后续像素错误掩盖原字体事实。
                        #[cfg(feature = "native-markdown-runtime-probe")]
                        eprintln!(
                            "native_line_font_size_mismatch phase=viewport part_points={} native_points={} core_points={:?} kind={:?}",
                            part.facts().points,
                            palette[font].native().point_size(),
                            palette[font].native().core_font().shaping_point_size(),
                            part.facts().kind,
                        );
                        return Err(NativeMarkdownLineGeometryError::FontSizeMismatch.into());
                    } // 字号与封存 part 一起认证；真实 TableRow 的零字号不属于文字整形合同。
                    let font_metrics = palette[font].metrics()?;
                    metrics
                        .add(
                            part.facts(),
                            font_metrics,
                            (key.presentation_mode
                                == omni_markdown_core::MarkdownPresentationMode::InlinePreview)
                                .then(|| palette[font].preview_metrics()),
                        )
                        .ok_or(NativeMarkdownComposedViewportError::InvalidGeometry)?;
                }
                glyphs = glyphs
                    .checked_add(part.facts().glyphs)
                    .ok_or(NativeMarkdownComposedViewportError::InvalidGeometry)?;
                let part = Part { handle: part, font };
                // 顺带消费本次已经认证的原 part 事实，不为后继每个 tile 再扫普通文字。
                // 负 advance 仍可能有背景面积，零 advance 和隐藏标记严格沿实际绘制谓词。
                has_inline_code_background |= part.has_inline_code_background();
                prepared.push(part);
            }
            if glyphs.checked_sub(glyphs_before) != Some(handle.facts().glyphs) {
                return Err(NativeMarkdownComposedViewportError::InvalidCoverage);
            }
            // 同行字体共用实际baseline；leading分在上下，下一行top只从真实度量累计。
            let geometry = if table_row {
                let height = handle.facts().y_advance_26_6 as f64 / 64.0;
                if !height.is_finite() || height <= 0.0 || height >= MAX_POINT {
                    return Err(NativeMarkdownComposedViewportError::InvalidGeometry);
                }
                // 这是同一行全部格真正完成后的块高度，不由注册字体的17pt行槽替代。
                super::preview_paragraph::PreviewLineGeometry {
                    advance: height,
                    baseline_from_top: 0.0,
                    text_height: height,
                    before: 0.0,
                    after: 0.0,
                }
            } else {
                metrics
                    .geometry(handle.facts())
                    .ok_or(NativeMarkdownComposedViewportError::InvalidGeometry)?
            };
            let height = geometry.advance;
            let baseline = CGPoint {
                x: domain.line_origin_x(
                    &key,
                    first_top.x,
                    metrics.paragraph.inset,
                    alignment_extent.as_ref(),
                )?,
                y: if key.presentation_mode == omni_markdown_core::MarkdownPresentationMode::Source
                {
                    top - metrics.ascent - metrics.leading / 2.0 // 字面 Source 保持原浮点运算次序。
                } else {
                    top - geometry.baseline_from_top
                },
            };
            let frame = NSRect::new(
                NSPoint::new(clip.origin.x, top - height),
                NSSize::new(clip.size.width, height),
            );
            if !point(baseline) || !rect(frame) {
                return Err(NativeMarkdownComposedViewportError::InvalidGeometry);
            } // 空字体行或非有限高度不能产生占位行框。
            parts += prepared.len();
            top -= height;
            let (ascent, descent) = if table_row {
                (0.0, 0.0) // 块行没有可用的文字插入点，真实格几何由Scene另行消费。
            } else if key.presentation_mode
                == omni_markdown_core::MarkdownPresentationMode::InlinePreview
            {
                let ascent = geometry.baseline_from_top - geometry.before;
                (ascent, geometry.text_height - ascent)
            } else {
                (metrics.ascent, metrics.descent) // Source保原CT选择/命中高度；Preview沿同一个实际行槽。
            };
            lines.push(Line {
                handle,
                parts: prepared,
                has_inline_code_background,
                frame,
                baseline,
                ascent,
                descent,
                paragraph: metrics.paragraph,
                geometry,
            });
        }
        if lines.is_empty() {
            return Err(NativeMarkdownComposedViewportError::Pending);
        } // 没有真实行的窗口不签Ready；空文档仍须真实控制part/字体生产者。
        // SAFETY: Core Graphics 的公开命名常量静态有效；只捕获色空间，不读取或复制正文。
        let paint_space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }));
        require_output(&output, &key, &domain, current)?; // Native 返回后仍让真实 Source 错误优先。
        let paint_space =
            paint_space.ok_or(NativeMarkdownComposedViewportError::ColorSpaceUnavailable)?;
        let candidate = Self {
            output,
            key,
            domain,
            palette,
            fonts: fonts.into_boxed_slice(),
            lines,
            glyphs,
            paint_space,
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

    #[cfg(feature = "native-markdown-runtime-probe")]
    /// 只观察已安装的至多256条原行；不推进布局、读取正文或重算 Native 排版。
    pub(in crate::native_surface) fn probe_line_geometry(
        &self,
        current: &dyn Fn() -> bool,
    ) -> Result<Vec<super::preview_paragraph::PreviewLineObservation>> {
        self.require(current)?;
        let values = self
            .lines
            .iter()
            .map(|line| {
                let facts = line.handle.facts();
                super::preview_paragraph::PreviewLineObservation {
                    source: facts.source_utf16.clone(),
                    paragraph: facts.paragraph_utf16.clone(),
                    frame: line.frame,
                    baseline: NSPoint::new(line.baseline.x, line.baseline.y),
                    text_height: line.geometry.text_height,
                    before: line.geometry.before,
                    after: line.geometry.after,
                }
            })
            .collect();
        self.require(current)?;
        Ok(values)
    }

    /// 封存时已经逐 part 认证真实字号与输出映射；复用只检查实际共享资源，不能按字形批次重扫整图。
    ///
    /// 原注册弱票按实际 Font 去重，源/Reader/publication 和每个 Font 的撤销仍须在回调前后生效。
    /// 新 draw 继续由原 part 读取检查其字形，零 glyph 的像素复用也不能绕过资源失败。
    pub fn validate_ready(&self, current: &dyn Fn() -> bool) -> Result<()> {
        self.require(current)?;
        for used in &self.fonts {
            let validation = (|| -> Result<()> {
                let font = &self.palette[used.index];
                font.require(&used.registration)?;
                font.metrics()?;
                Ok(())
            })();
            self.require(current)?; // Native/current 可重入；原 Source 首错先于刚取得的字体失败。
            validation?;
        }
        self.require(current)
    }

    /// 原窗口的单项消费只核同一Source/Reader/publication；字体全图复核留在量子边界。
    pub(in crate::native_surface) fn require(&self, current: &dyn Fn() -> bool) -> Result<()> {
        require_output(&self.output, &self.key, &self.domain, current)
    }

    /// 所有 glyph/GCB/几何读取都沿同一实际布局来源；外层 SceneKey 继续表示真实 canonical 场景。
    fn source(&self) -> MarkdownLayoutTextAuthority {
        self.domain.source(&self.key)
    }

    /// 范围门沿当前实际布局域，不能用外层正文长度验证显示格局部端点。
    fn utf16_len(&self) -> usize {
        self.domain.utf16_len(&self.key)
    }

    /// 绘图、命中和选择共用同一次实际视口交集。
    fn clip(&self) -> NSRect {
        self.domain.clip(&self.key)
    }

    fn visible(&self, frame: NSRect) -> bool {
        intersects(frame, self.clip())
    }
}

/// 原Source故障必须压过bool撤权；current两边都归还Reader再调用，不让重入掩盖EIO。
fn require_output(
    output: &RefCell<MarkdownComposedLines>,
    key: &NativeMarkdownPagedSceneKey,
    domain: &LayoutDomain,
    current: &dyn Fn() -> bool,
) -> Result<()> {
    let before = current();
    {
        let reader = output
            .try_borrow()
            .map_err(|_| NativeMarkdownComposedViewportError::Busy)?;
        reader.require(&domain.source(key))?;
    }
    domain.require(key)?;
    let after = current(); // 首次检查可能重入；下面仍读原Source，不通过新capture恢复权限。
    {
        let reader = output
            .try_borrow()
            .map_err(|_| NativeMarkdownComposedViewportError::Busy)?;
        reader.require(&domain.source(key))?;
    }
    domain.require(key)?;
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
