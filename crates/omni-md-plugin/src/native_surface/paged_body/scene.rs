//! 原已封存窗口的输入和绘图消费；任何方法都不创建新 Source 或推进排版。

use std::{
    cell::{OnceCell, RefCell},
    ops::Range,
    rc::{Rc, Weak},
};

use objc2_core_foundation::CFRetained;
use objc2_core_graphics::{CGColorSpace, CGContext, kCGColorSpaceSRGB};
use objc2_foundation::{NSPoint, NSRange, NSRect, NSSize};
use omni_markdown_core::{
    MarkdownComposedLines, MarkdownGraphemeAnswer, MarkdownGraphemeQuery, MarkdownTextSelection,
};

use super::producer::scene_error;
use super::{
    NativeMarkdownPagedBody as Owner, NativeMarkdownPagedBodyColors, NativeMarkdownPagedSceneKey,
    NativeMarkdownPagedTextScene, ViewError as Error,
};
use crate::native_surface::{
    gutter::paged::GutterRow,
    paged_input::{NativeMarkdownBidiCaretSide, NativeMarkdownVerticalGoal},
    paged_line_viewport::{
        NativeMarkdownComposedViewport, NativeMarkdownComposedViewportError as ViewportError,
        NativeMarkdownComposedViewportPaintBudget, NativeMarkdownComposedViewportPaintCause,
        NativeMarkdownComposedViewportPaintError, NativeMarkdownComposedViewportPaintOutcome,
        NativeMarkdownComposedViewportPaintSession,
    },
    paged_text_view::{
        NativeMarkdownPagedCharacterRect, NativeMarkdownPagedEditCommand as Edit,
        NativeMarkdownPagedEditPlan, NativeMarkdownPagedHeadAffinity as Affinity,
        NativeMarkdownPagedNavigation as Nav, NativeMarkdownPagedNavigationContext,
        NativeMarkdownPagedNavigationGeometry, NativeMarkdownPagedNavigationTarget,
        NativeMarkdownPagedSceneNonce, NativeMarkdownPagedScenePaintBudget,
        NativeMarkdownPagedScenePaintCompletion, NativeMarkdownPagedScenePaintOutcome,
        NativeMarkdownPagedScenePaintSession, NativeMarkdownPagedScenePaintStep,
        NativeMarkdownPagedScenePaintWork, NativeMarkdownPagedTextViewDelegate,
    },
    paged_viewport::NativeMarkdownPagedViewportGeometry,
};

type Result<T> = std::result::Result<T, Error>;
mod grid;
mod paint;
use grid::GridCharacterGeometry;
pub(super) use grid::{GridCellContent, GridCellPublication, GridRowPublication};
use paint::Paint;

pub(in crate::native_surface) struct Scene {
    key: NativeMarkdownPagedSceneKey,
    _nonce: NativeMarkdownPagedSceneNonce,
    geometry: NativeMarkdownPagedViewportGeometry,
    window: Option<Rc<NativeMarkdownComposedViewport>>,
    output: Option<Rc<RefCell<MarkdownComposedLines>>>,
    grids: Box<[GridRowPublication]>,
    owner: Weak<Owner>,
    ranges: Box<[Range<usize>]>,
    paragraphs: Box<[Range<usize>]>,
    folded_ends: Box<[Option<usize>]>,
    simple_ltr: bool,
    navigation: NativeMarkdownPagedNavigationGeometry,
    colors: NativeMarkdownPagedBodyColors,
    head_affinity: Option<Affinity>,
    first_ordinal: usize,
    /// 仅保存发布时同一输入票的有限范围；高亮不能用主范围冒充全部选中。
    selections: Box<[Range<usize>]>,
    /// 当前查询在真实可见源域中的有限匹配；不保栏、查询或Native强引用。
    find_highlights: Box<[Range<usize>]>,
    /// 仅原Source单有向选择的canonical逻辑行；UBA段和soft line均不授予此范围。
    active_line: Option<Range<usize>>,
    /// Source同次发布的有限行号；真实软行几何与canonical逻辑行只在Producer相交一次。
    gutter: Option<Box<[GutterRow]>>,
    // Scene 发布不创建 Native 资源；首个实际绘图入口捕获一次命名空间，随本 Scene 释放。
    // None 是该次真实创建失败，不能改成 DeviceRGB 默认空间继续画出另一种颜色。
    paint_space: OnceCell<Option<CFRetained<CGColorSpace>>>,
}

/// 一次准备结束的有限字段集合；构造范围仅限 Body，外界不能凭范围制造 Ready。
pub(super) struct Publication {
    pub key: NativeMarkdownPagedSceneKey,
    pub nonce: NativeMarkdownPagedSceneNonce,
    pub geometry: NativeMarkdownPagedViewportGeometry,
    pub window: Option<Rc<NativeMarkdownComposedViewport>>,
    pub output: Option<Rc<RefCell<MarkdownComposedLines>>>,
    pub grids: Box<[GridRowPublication]>,
    pub owner: Weak<Owner>,
    pub ranges: Box<[Range<usize>]>,
    pub paragraphs: Box<[Range<usize>]>,
    pub folded_ends: Box<[Option<usize>]>,
    pub simple_ltr: bool,
    pub width_key: i64,
    pub layout_epoch: u64,
    pub colors: NativeMarkdownPagedBodyColors,
    pub head_affinity: Option<Affinity>,
    pub first_ordinal: usize,
    pub selections: Box<[Range<usize>]>,
    pub find_highlights: Box<[Range<usize>]>,
    pub active_line: Option<Range<usize>>,
    pub gutter: Option<Box<[GutterRow]>>,
}

impl Scene {
    #[cfg(feature = "native-markdown-runtime-probe")]
    /// 原帧的弱身份仍有效才交付有限几何；短 oracle 不取得这个 Scene 的长期强引用。
    pub(super) fn probe_line_geometry(
        &self,
    ) -> Result<Vec<crate::native_surface::preview_paragraph::PreviewLineObservation>> {
        self.require()?;
        self.text_window()?
            .probe_line_geometry(&|| self.current())
            .map_err(scene_error)
    }

    pub(super) fn new(publication: Publication) -> Self {
        let Publication {
            key,
            nonce,
            geometry,
            window,
            output,
            grids,
            owner,
            ranges,
            paragraphs,
            folded_ends,
            simple_ltr,
            width_key,
            layout_epoch,
            colors,
            head_affinity,
            first_ordinal,
            selections,
            find_highlights,
            active_line,
            gutter,
        } = publication;
        let navigation = NativeMarkdownPagedNavigationGeometry {
            source: key.source.clone(),
            width_key,
            layout_epoch,
        };
        Self {
            key,
            _nonce: nonce,
            geometry,
            window,
            output,
            grids,
            owner,
            ranges,
            paragraphs,
            folded_ends,
            simple_ltr,
            navigation,
            colors,
            head_affinity,
            first_ordinal,
            selections,
            find_highlights,
            active_line,
            gutter,
            paint_space: OnceCell::new(),
        }
    }

    /// Source 与 Ready 核验包住实际色空间捕获；创建失败沿绘图错误返回，不能冒充几何失败。
    fn prepare_paint_space(&self, current: &dyn Fn() -> bool) -> Result<()> {
        self.validate_ready(current)?;
        let space = self.paint_space.get_or_init(|| {
            // SAFETY: 公开 sRGB 名称静态有效；同一个 Scene 不为每次颜色或 glyph 重新分配对象。
            CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))
        });
        self.validate_ready(current)?;
        if space.is_none() {
            return Err(Error::Raster(
                ViewportError::ColorSpaceUnavailable.to_string(),
            ));
        }
        Ok(())
    }

    /// 调色板声明的是源 sRGB 分量，须显式设置输入空间；位图的目标空间不能替代此事实。
    fn color(&self, context: &CGContext, color: [f64; 4]) {
        let space = self
            .paint_space
            .get()
            .and_then(Option::as_deref)
            .expect("Scene 实际绘图先取得命名 sRGB 空间");
        CGContext::set_fill_color_space(Some(context), Some(space));
        // SAFETY: 命名 sRGB 有三个颜色分量，本同步栈的数组另含一个 alpha，恰好满足公开 API。
        unsafe { CGContext::set_fill_color(Some(context), color.as_ptr()) };
    }

    /// 边线使用同一份源色空间；不调用会把输入重新切回 DeviceRGB 的 convenience setter。
    fn stroke_color(&self, context: &CGContext, color: [f64; 4]) {
        let space = self
            .paint_space
            .get()
            .and_then(Option::as_deref)
            .expect("Scene 实际绘图先取得命名 sRGB 空间");
        CGContext::set_stroke_color_space(Some(context), Some(space));
        // SAFETY: 当前 stroke 空间同为三分量 sRGB，四个栈分量完整有效到原调用返回。
        unsafe { CGContext::set_stroke_color(Some(context), color.as_ptr()) };
    }

    /// 模式捕获只检查已封存窗口中的有限候选，离屏caret不能替代真实可见字形。
    pub(super) fn mode_source_candidates(&self) -> impl Iterator<Item = Result<usize>> + '_ {
        let head = self.key.request.input.primary_range.location;
        let visible = self.window.as_ref().is_some_and(|window| {
            let source = window.source_range();
            source.start <= head && head <= source.end
        }) || self.grids.iter().any(|row| {
            let range = row.row.source_range();
            range.start <= head && head <= range.end
        });
        std::iter::once(head)
            .filter(move |_| visible)
            .map(Ok)
            .chain(self.ranges.iter().map(|range| Ok(range.start)))
            .chain(
                self.grids
                    .iter()
                    .flat_map(|row| row.cells.iter())
                    .filter_map(|cell| {
                        let GridCellContent::Composed(window) = &cell.content else {
                            return None;
                        };
                        match window.first_visible_source(&|| self.current()) {
                            Ok(value) => value.map(Ok),
                            Err(cause) => Some(Err(scene_error(cause))),
                        }
                    }),
            )
    }

    /// 行号借用原Scene的全部Source/输入/字体/几何权限，不创建新的Ready或读取正文。
    pub(in crate::native_surface) fn gutter_rows(&self) -> Result<&[GutterRow]> {
        self.require()?;
        self.gutter.as_deref().ok_or(Error::Revoked)
    }

    /// 捕获原caret的真实行/目标列；坐标只从当前有限窗口的实际Native字体度量转换。
    pub(super) fn vertical_seed(
        &self,
        head: usize,
        context: &NativeMarkdownPagedNavigationContext,
        keep_goal: bool,
    ) -> Result<super::navigation::VerticalSeed> {
        self.require()?;
        if !context.geometry.same(&self.navigation) {
            return Err(Error::Revoked);
        }
        let caret = self
            .text_window()?
            .caret_geometry_with_side(
                head,
                context.head_affinity.unwrap_or(Affinity::Downstream),
                context.bidi_side,
                &|| self.current(),
            )
            .map_err(scene_error)?;
        if caret.secondary.is_some() {
            return Err(Error::UnsupportedCommand);
        }
        let bottom = self.key.request.virtual_viewport.document_origin.y
            + self.key.request.viewport.size.height
            - caret.line_frame.origin.y;
        let top = bottom - caret.line_frame.size.height;
        let old_goal = context
            .vertical_goal
            .filter(|goal| self.navigation.permits_goal(*goal));
        let x = old_goal.map_or(caret.primary.origin.x, |goal| goal.container_x());
        let goal = if keep_goal {
            old_goal.or_else(|| {
                NativeMarkdownVerticalGoal::new(
                    caret.primary.origin.x - caret.line_frame.origin.x,
                    caret.primary.origin.x,
                    self.navigation.width_key,
                    self.navigation.layout_epoch,
                )
            })
        } else {
            None
        }; // Page仍沿原行为使用当前列；不把它变为连续Up/Down目标。
        self.require()?;
        Ok(super::navigation::VerticalSeed {
            ordinal: caret.line_ordinal,
            top,
            bottom,
            caret_y: self.key.request.virtual_viewport.document_origin.y
                + self.key.request.viewport.size.height
                - caret.primary.origin.y
                - caret.primary.size.height * 0.5,
            x,
            goal,
        })
    }

    /// 只命中Producer已度量并真正滚入当前窗口的那一行；越窗不夹到边缘行。
    pub(super) fn vertical_line_target(
        &self,
        ordinal: usize,
        x: f64,
        before: MarkdownTextSelection,
        extend: bool,
        goal: Option<NativeMarkdownVerticalGoal>,
    ) -> Result<NativeMarkdownPagedNavigationTarget> {
        self.require()?;
        let index = ordinal
            .checked_sub(self.first_ordinal)
            .filter(|index| *index < self.ranges.len())
            .ok_or(Error::Pending)?;
        let frame = self
            .text_window()?
            .line_frame(index)
            .ok_or(Error::InvalidGeometry)?;
        let viewport = self.key.request.viewport;
        let bottom = frame.origin.y.max(viewport.origin.y);
        let top =
            (frame.origin.y + frame.size.height).min(viewport.origin.y + viewport.size.height);
        if bottom >= top {
            return Err(Error::Pending);
        }
        let hit = self
            .text_window()?
            .hit(
                NSPoint::new(
                    x.clamp(
                        viewport.origin.x,
                        (viewport.origin.x + viewport.size.width).next_down(),
                    ),
                    bottom + (top - bottom) * 0.5,
                ),
                &|| self.current(),
            )
            .map_err(scene_error)?;
        if hit.line_ordinal != ordinal || hit.other_source_utf16.is_some() {
            return Err(Error::UnsupportedCommand);
        }
        let affinity = if hit.source_utf16 == self.ranges[index].end {
            Affinity::Upstream
        } else {
            Affinity::Downstream
        };
        self.require()?;
        Ok(NativeMarkdownPagedNavigationTarget {
            selection: MarkdownTextSelection {
                anchor_utf16: if extend {
                    before.anchor_utf16
                } else {
                    hit.source_utf16
                },
                head_utf16: hit.source_utf16,
            },
            head_affinity: Some(affinity),
            vertical_goal: goal,
            bidi_side: None,
        }) // 混合Shift从同一原票的working anchor接续，不暗取尚未提交的canonical初始anchor。
    }

    fn current(&self) -> bool {
        self.geometry.is_current()
            && self
                .owner
                .upgrade()
                .is_some_and(|owner| owner.scene_current(&self.key))
    }

    /// Scene只借原Input唯一选择槽；同key/serial和原scope前后都必须仍有效。
    fn bidi_side(&self) -> Result<Option<NativeMarkdownBidiCaretSide>> {
        self.require()?;
        let owner = self.owner.upgrade().ok_or(Error::Revoked)?;
        let current = |epoch| owner.current(epoch) && self.current();
        let permit = owner.input.capture(owner.configuration.epoch, &current)?;
        if owner.input.state()? != self.key.request.input {
            return Err(Error::Revoked);
        }
        let side = owner.input.navigation_bidi_side(&permit, &current)?;
        self.require()?;
        Ok(side)
    }

    /// Grid-only 前缀没有文字行；文字专用几何保持真实 Pending，不能造空文字窗口补票。
    fn text_window(&self) -> Result<&Rc<NativeMarkdownComposedViewport>> {
        self.window.as_ref().ok_or(Error::Pending)
    }

    fn require(&self) -> Result<()> {
        self.validate_ready(&|| self.current())
    }

    fn selection(&self) -> Result<MarkdownTextSelection> {
        self.key
            .request
            .input
            .directed_selection()
            .ok_or(Error::InvalidRange)
    }

    fn line(&self, position: usize, affinity: Affinity) -> Result<usize> {
        self.ranges
            .iter()
            .position(|range| {
                if position == self.key.request.input.utf16_len {
                    position == range.end
                } else if affinity.is_upstream() && position != 0 {
                    range.start < position && position <= range.end
                } else {
                    range.start <= position && position < range.end
                }
            })
            .ok_or(Error::InvalidRange)
    }

    /// canonical 行覆盖仍含整个 opaque 区；只有行内动作使用同次 glyph/control 证明的可见末端。
    fn visible_line_range(&self, line: usize) -> Range<usize> {
        let range = &self.ranges[line];
        range.start..self.folded_ends[line].unwrap_or(range.end)
    }

    fn boundary(&self, position: usize, next: bool) -> Result<usize> {
        self.require()?;
        let answer = {
            let mut output = self
                .output
                .as_ref()
                .ok_or(Error::Pending)?
                .try_borrow_mut()
                .map_err(|_| Error::Busy)?;
            output
                .grapheme_query(
                    position,
                    if next {
                        MarkdownGraphemeQuery::Next
                    } else {
                        MarkdownGraphemeQuery::Previous
                    },
                )
                .map_err(|cause| scene_error(cause.into()))?
        };
        self.require()?;
        match answer {
            MarkdownGraphemeAnswer::Known(Some(boundary)) => Ok(boundary.utf16),
            MarkdownGraphemeAnswer::Known(None) if !next && position == 0 => Ok(0),
            MarkdownGraphemeAnswer::Known(None)
                if next && position == self.key.request.input.utf16_len =>
            {
                Ok(position)
            }
            // 缓存 EOF 没有全文 EOF 的语义。只有主动排版在途才可 Pending，否则明确范围不可用。
            _ => Err(self.missing_geometry()),
        }
    }

    fn missing_geometry(&self) -> Error {
        if self
            .owner
            .upgrade()
            .is_some_and(|owner| owner.is_preparing())
        {
            Error::Pending
        } else {
            Error::InvalidRange
        }
    }

    fn target(
        &self,
        head: usize,
        extend: bool,
        affinity: Option<Affinity>,
        goal: Option<NativeMarkdownVerticalGoal>,
    ) -> Result<NativeMarkdownPagedNavigationTarget> {
        self.require()?;
        let before = self.selection()?;
        let selection = MarkdownTextSelection {
            anchor_utf16: if extend { before.anchor_utf16 } else { head },
            head_utf16: head,
        };
        Ok(NativeMarkdownPagedNavigationTarget {
            selection,
            head_affinity: affinity,
            vertical_goal: goal,
            bidi_side: None,
        })
    }

    /// 同步对照保原顺序：整背景、Grid底色边线、正文装饰、Grid选区，最后才绘制字形。
    fn decorations(&self, context: &CGContext, current: &dyn Fn() -> bool) -> Result<()> {
        self.fill_canvas(context, current)?;
        self.grid_decorations(context, &|| current() && self.current())?;
        self.text_decorations(context, current)?;
        self.grid_selection(context, &|| current() && self.current())?;
        self.color(context, self.colors.text);
        self.validate_ready(current)
    }

    /// 候选画布只填一次固定视口；Grid的逐项工作留给原Paint续体，不在构造栈扫描各格。
    fn fill_canvas(&self, context: &CGContext, current: &dyn Fn() -> bool) -> Result<()> {
        self.validate_ready(current)?;
        self.color(context, self.colors.background);
        CGContext::fill_rect(Some(context), self.key.request.viewport);
        self.validate_ready(current)
    }

    /// 正文保既有单窗口装饰路径和像素顺序；Source没有Grid时原入口行为不变。
    fn text_decorations(&self, context: &CGContext, current: &dyn Fn() -> bool) -> Result<()> {
        self.validate_ready(current)?;
        if let Some(window) = &self.window {
            window
                .paint_code_backgrounds(context, &|| current() && self.current())
                .map_err(paint_error)?;
        } // 原代码底色先落，再画活动行和真实选区；同步与CPU候选共用此顺序。
        if let Some(active) = &self.active_line {
            self.color(context, self.colors.active_line);
            for (index, range) in self.ranges.iter().enumerate() {
                let selected = if active.is_empty() {
                    range.is_empty() && range.start == active.start
                } else {
                    range.start < active.end && active.start < range.end
                };
                if !selected {
                    continue; // 不同逻辑行或真实EOF尾空行不能借前一wrapped行的范围着色。
                }
                let frame = self
                    .text_window()?
                    .line_frame(index)
                    .ok_or(Error::InvalidGeometry)?;
                let viewport = self.key.request.viewport;
                let bottom = frame.origin.y.max(viewport.origin.y);
                let top = (frame.origin.y + frame.size.height)
                    .min(viewport.origin.y + viewport.size.height);
                if top <= bottom {
                    continue; // 实际已度量但在overscan之外的行没有本次像素后果。
                }
                // 行框已由原window覆盖viewport全宽；只裁真实垂直交集，不按平均行高猜矩形。
                CGContext::fill_rect(
                    Some(context),
                    NSRect::new(
                        NSPoint::new(frame.origin.x, bottom),
                        NSSize::new(frame.size.width, top - bottom),
                    ),
                );
            }
            self.validate_ready(current)?;
        } // 活动行先于选区和文字绘制；不遮掉原选中反馈，也不读取或重排离屏正文。
        // 查找背景与分步路径一样位于代码/活动行之后、用户选区之前。
        // 同步入口保独立的真实窗口循环，不能漏掉查找反馈后再拿缺色图当像素期望。
        self.find_decorations(context, current)?;
        if let Some(window) = &self.window {
            let prepared = window.source_range();
            // 输入所有者已经认证范围有序且不重叠；二分跳过视口前的范围，不扫描其正文。
            // 这里只借发布时的整数元数据，离屏匹配不会推进排版或使匹配包络进入内存。
            let first = self
                .selections
                .partition_point(|selection| selection.end <= prepared.start);
            self.color(context, self.colors.selection);
            for selection in &self.selections[first..] {
                if selection.start >= prepared.end {
                    break; // 后续范围同样位于本次准备窗口之后，没有本帧绘图后果。
                }
                if selection.is_empty() {
                    continue; // 空范围是光标位置，不能制造额外的选中背景。
                }
                // 所有范围共用同一封存窗口和候选画布；任一真实几何或权限错误使整帧失败。
                window
                    .selection_rects(selection.clone(), current, |rect| {
                        CGContext::fill_rect(Some(context), rect);
                        Ok(())
                    })
                    .map_err(scene_error)?;
            }
        }
        self.color(context, self.colors.text);
        self.validate_ready(current)
    }

    /// 只画本次已封存可见匹配；每个框由原窗口映射并裁剪，不创建第二份Find或排版结果。
    /// 正文与格分别消费各自Source，透明色只覆盖一次真实矩形，不能叠加同一边缘clip。
    fn find_decorations(&self, context: &CGContext, current: &dyn Fn() -> bool) -> Result<()> {
        self.validate_ready(current)?;
        if self.find_highlights.is_empty() {
            return Ok(()); // 没有实际匹配就没有绘图副作用，也不枚举当前Grid。
        }
        self.color(context, [1.0, 0.75, 0.12, 0.25]);
        if let Some(window) = &self.window {
            let prepared = window.source_range();
            let first = self
                .find_highlights
                .partition_point(|range| range.end <= prepared.start);
            for range in &self.find_highlights[first..] {
                if range.start >= prepared.end {
                    break;
                }
                window
                    .selection_rects(range.clone(), current, |frame| {
                        CGContext::fill_rect(Some(context), frame);
                        Ok(())
                    })
                    .map_err(scene_error)?;
            }
        }
        // 真实格映射只裁本格交集，隐藏Markdown语法不产生面积；离屏匹配不推进任何来源。
        // 与异步阶段保持正文Find→Grid Find→正文选区→Grid选区的相同覆盖顺序。
        for row in &self.grids {
            let source = row.row.source_range();
            let first = self
                .find_highlights
                .partition_point(|range| range.end <= source.start);
            for range in &self.find_highlights[first..] {
                if range.start >= source.end {
                    break;
                }
                for cell in &row.cells {
                    if let GridCellContent::Composed(window) = &cell.content {
                        window
                            .selection_rects(range.clone(), current, |frame| {
                                CGContext::fill_rect(Some(context), frame);
                                Ok(())
                            })
                            .map_err(scene_error)?;
                    }
                }
            }
        }
        self.validate_ready(current)
    }

    fn caret(&self, context: &CGContext, current: &dyn Fn() -> bool) -> Result<()> {
        self.validate_ready(current)?;
        let input = self.key.request.input;
        if input.selection_range_count != 1 || input.primary_range.length != 0 {
            return Ok(());
        }
        let selection = self.selection()?;
        let affinity = self.head_affinity.unwrap_or(Affinity::Downstream);
        match self.grid_first_rect(NSRange::new(selection.head_utf16, 0), affinity)? {
            GridCharacterGeometry::Ready(value) => {
                let mut rect = value.rect;
                rect.size.width = 1.0;
                self.color(context, self.colors.caret);
                CGContext::fill_rect(Some(context), rect);
                return self.validate_ready(current);
            }
            GridCharacterGeometry::Unavailable => return self.validate_ready(current),
            GridCharacterGeometry::Outside => {}
        } // 隐藏语法仍属于表格；没有可见格时不退到零字号TableRow结构part。

        let Some(window) = &self.window else {
            return self.validate_ready(current);
        };
        let prepared = window.source_range();
        if selection.head_utf16 < prepared.start || selection.head_utf16 > prepared.end {
            return Ok(()); // 离屏 caret 没有本次绘图后果，不为它重排或读取其他段落。
        }
        if window
            .table_row_at(selection.head_utf16, affinity, &|| {
                current() && self.current()
            })
            .map_err(scene_error)?
        {
            return self.validate_ready(current); // 原正文overscan中的离屏结构行也没有普通文字caret。
        }
        let geometry = self
            .text_window()?
            .caret_geometry_with_side(
                selection.head_utf16,
                self.head_affinity.unwrap_or(Affinity::Downstream),
                self.bidi_side()?,
                &|| current() && self.current(),
            )
            .map_err(scene_error)?;
        if geometry.secondary.is_some() {
            return Err(Error::UnsupportedCommand);
        }
        let mut rect = geometry.primary;
        rect.size.width = 1.0;
        self.color(context, self.colors.caret);
        CGContext::fill_rect(Some(context), rect);
        self.validate_ready(current)
    }
}

impl NativeMarkdownPagedTextScene for Scene {
    fn key(&self) -> &NativeMarkdownPagedSceneKey {
        &self.key
    }
    /// 正文与只读格仍借同一张Scene；只交当前sealed标签的矩形，不登记URL或持久几何表。
    /// visit只收集Copy矩形；调用者须归还这张Scene后才可进入Native，返回前仍读原Source首错。
    fn visit_link_cursor_rects(&self, visit: &mut dyn FnMut(NSRect)) -> Result<()> {
        self.require()?;
        if let Some(window) = &self.window {
            window
                .visit_link_cursor_rects(&|| self.current(), visit)
                .map_err(scene_error)?;
        }
        self.visit_grid_link_cursor_rects(visit)?;
        self.require()
    }
    /// 原sealed LINK part与真实GCB侧只授命中事实，不同步推进语义或把marker当作label。
    fn link_hit(&self, local: NSPoint) -> Result<Option<(Range<usize>, usize)>> {
        self.require()?;
        if self.grid_cell_at(local).is_some() {
            return self.grid_link_hit(local);
        }
        if self.window.is_none() {
            return Ok(None);
        }
        let Some(part) = self
            .text_window()?
            .link_part_at(local, &|| self.current())
            .map_err(scene_error)?
        else {
            return Ok(None); // 普通文本不触发额外来源查询，行尾空白和显露marker也不截获原点击。
        };
        let (head, backward, _) = self.word_hit(local)?;
        let index = if backward {
            self.boundary(head, false)?
        } else {
            head
        };
        if !part.contains(&index) {
            return Ok(None); // 最近caret可位于链接边缘之外；只有实际命中part中的原GCB才拥有激活权限。
        }
        self.require()?;
        Ok(Some((part, index)))
    }
    /// 原mouseup意图移交唯一Body；Scene不持后继闭包，也不在指针栈排版或打开外部应用。
    fn follow_link(&self, index: usize, intent: Weak<()>) -> Result<()> {
        self.require()?;
        let owner = self.owner.upgrade().ok_or(Error::Revoked)?;
        owner.follow_link(&self.key, index, intent)
    }
    /// 原Scene先核Source/Font/输出健康，再把fragment交回同一弱Body；
    /// 不经Workspace全局槽寻找另一个正文，也不在鼠标回调同步扫描整个文稿。
    fn follow_heading(&self, fragment: &str, intent: Weak<()>) -> Result<()> {
        self.require()?;
        let owner = self.owner.upgrade().ok_or(Error::Revoked)?;
        owner.follow_heading(&self.key, intent, fragment)
    }
    /// 先证原可见链接part和真实GCB侧，再从同一Source/选择/publication取得有限目标副本。
    fn link_target(&self, local: NSPoint) -> Result<Option<String>> {
        let Some((_, index)) = self.link_hit(local)? else {
            return Ok(None);
        };
        let owner = self.owner.upgrade().ok_or(Error::Revoked)?;
        let link = {
            let producer = owner.producer.borrow();
            producer
                .as_ref()
                .ok_or(Error::Pending)?
                .link_at(&owner, &self.key, index)?
        }; // 原Doc/Producer借用已结束，后继Native URL初始化不跨Core资源借用。
        self.require()?;
        // 此处是原指针URL查询出口，已退还Doc/Producer借用并核同一key；有限目标的连续
        // 表示只交给后继Native URL调用，不缓存为第二份CPU语义或借旧Source补权。
        Ok(link.map(|link| link.destination.into_string()))
    }
    fn cpu_frame_published(&self, paints: usize) {
        // CPU owner 已真正提交完整帧；这里只排同一个原 key 的弱通知，不在绘图栈调用外方。
        if let Some(owner) = self.owner.upgrade() {
            owner.queue_cpu_publication(&self.key, paints);
        }
    }
    fn validate_ready(&self, current: &dyn Fn() -> bool) -> Result<()> {
        // 先核所有真实派生源；正文普通撤权不能把另一个格的目录首错覆盖为取消。
        for row in &self.grids {
            for cell in &row.cells {
                if let GridCellContent::Composed(window) = &cell.content {
                    window.source_health().map_err(scene_error)?;
                }
            }
        }
        if let Some(window) = &self.window {
            window
                .validate_ready(&|| current() && self.current())
                .map_err(scene_error)?;
        }
        self.validate_grids(&|| current() && self.current())?;
        if self.window.is_none() && self.grids.is_empty() {
            return Err(Error::Pending);
        }
        Ok(())
    }
    fn first_rect(&self, range: NSRange) -> Result<NativeMarkdownPagedCharacterRect> {
        self.require()?; // 包括Grid域内无可见格的结果；Source首错不能被空间Pending掩盖。
        let affinity =
            if range.length == 0 && range.location == self.key.request.input.selection.head_utf16 {
                self.head_affinity.unwrap_or(Affinity::Downstream)
            } else {
                Affinity::Downstream
            };
        match self.grid_first_rect(range, affinity)? {
            GridCharacterGeometry::Ready(rect) => return Ok(rect),
            GridCharacterGeometry::Unavailable => {
                self.require()?; // 空间不可用也先保原Source首错，不把异步读取失败改写成Pending。
                return Err(Error::Pending);
            }
            GridCharacterGeometry::Outside => {}
        }
        let geometry_error = |cause| match cause {
            ViewportError::Pending => Error::Pending,
            other => scene_error(other),
        }; // 行域查询与实际字符查询同属此公开边界，屏外/尚未准备都保原Pending语义。
        let window = self.text_window()?;
        if range.length == 0
            && window
                .table_row_at(range.location, affinity, &|| self.current())
                .map_err(geometry_error)?
        {
            return Err(Error::Pending); // 可见Grid之外的结构行不借目录中的font0合成文字矩形。
        }
        window
            .first_rect_with_caret_side(range, affinity, self.bidi_side()?, &|| self.current())
            .map_err(geometry_error)
    }
    fn hit(&self, local: NSPoint) -> Result<usize> {
        self.require()?;
        if let Some(position) = self.grid_hit(local)? {
            return Ok(position);
        }
        let hit = self
            .text_window()?
            .hit(local, &|| self.current())
            .map_err(scene_error)?;
        if hit.other_source_utf16.is_some() {
            return Err(Error::UnsupportedCommand);
        }
        Ok(hit.source_utf16)
    }
    fn drag_selection_point(&self, local: NSPoint) -> Result<NSPoint> {
        self.require()?;
        let mut nearest = None;
        let mut distance = f64::INFINITY;
        let mut consider = |point: NSPoint| {
            let delta = (local.x - point.x).hypot(local.y - point.y);
            if delta < distance {
                nearest = Some(point);
                distance = delta;
            }
        };
        if let Some(window) = &self.window
            && let Some(point) = window
                .project_drag_point(local, &|| self.current())
                .map_err(scene_error)?
        {
            consider(point);
        }
        for row in &self.grids {
            for cell in &row.cells {
                if let GridCellContent::Composed(window) = &cell.content {
                    if let Some(point) = window
                        .project_drag_point(local, &|| self.current())
                        .map_err(scene_error)?
                    {
                        consider(point);
                    }
                } else {
                    // 空格、文字前/后padding只有原格框能授予真实端点；不用整行等分推测列位置。
                    let clip = self.key.request.viewport;
                    let left = cell.frame.origin.x.max(clip.origin.x);
                    let right = (cell.frame.origin.x + cell.frame.size.width)
                        .min(clip.origin.x + clip.size.width);
                    let bottom = cell.frame.origin.y.max(clip.origin.y);
                    let top = (cell.frame.origin.y + cell.frame.size.height)
                        .min(clip.origin.y + clip.size.height);
                    if right > left && top > bottom {
                        consider(NSPoint::new(
                            local.x.clamp(left, right.next_down()),
                            local.y.clamp(bottom, top.next_down()),
                        ));
                    }
                }
            }
        }
        self.require()?;
        nearest.ok_or(Error::Pending)
    }
    fn cell_hit(
        &self,
        local: NSPoint,
        word: bool,
    ) -> Result<Option<super::super::paged_text_view::NativeMarkdownPagedCellHit>> {
        self.require()?;
        let hit = self.grid_pointer_hit(local, word)?;
        self.require()?;
        Ok(hit)
    }

    fn paragraph_selection(
        &self,
        position: usize,
        affinity: Affinity,
    ) -> Result<MarkdownTextSelection> {
        self.require()?;
        let line = self.line(position, affinity)?;
        let range = &self.paragraphs[line];
        self.require()?;
        Ok(MarkdownTextSelection {
            anchor_utf16: range.start,
            head_utf16: range.end,
        })
    }

    fn word_hit(
        &self,
        local: NSPoint,
    ) -> Result<(
        usize,
        bool,
        Option<crate::native_surface::paged_text_view::NativeMarkdownPagedCellWordTarget>,
    )> {
        self.require()?;
        if let Some(hit) = self.grid_pointer_hit(local, true)? {
            self.require()?;
            return Ok((hit.head_utf16, hit.backward_bias, hit.word_target));
        }
        let (hit, backward_bias) = self
            .text_window()?
            .word_hit(local, &|| self.current())
            .map_err(scene_error)?;
        self.require()?;
        Ok((hit.source_utf16, backward_bias, None))
    }

    fn mouse_selection(
        &self,
        local: NSPoint,
        clicks: usize,
        anchor: Option<usize>,
        extend: bool,
    ) -> Result<(MarkdownTextSelection, Affinity)> {
        self.require()?;
        let (position, affinity) = if let Some(hit) = self.grid_pointer_hit(local, false)? {
            (hit.head_utf16, hit.head_affinity)
        } else {
            let hit = self
                .text_window()?
                .hit(local, &|| self.current())
                .map_err(scene_error)?;
            if hit.other_source_utf16.is_some() {
                return Err(Error::Pending);
            }
            (
                hit.source_utf16,
                Affinity::from_upstream(
                    hit.source_affinity
                        == omni_markdown_core::MarkdownComposedLineAffinity::Upstream,
                ),
            )
        }; // 实际命中侧与选择一并交付；跨格提交不能用固定Downstream覆盖软换行上游。
        let before = self.selection()?;
        let selection = match clicks {
            0 | 1 => MarkdownTextSelection {
                anchor_utf16: anchor.unwrap_or(if extend {
                    before.anchor_utf16
                } else {
                    position
                }),
                head_utf16: position,
            },
            3 => self.paragraph_selection(position, affinity)?,
            // 双击分词需要真实 word producer；不按 ASCII 空格猜中文和标点的选择范围。
            _ => return Err(Error::UnsupportedCommand),
        };
        self.require()?;
        Ok((
            selection,
            if clicks == 3 {
                Affinity::Upstream
            } else {
                affinity
            },
        ))
    }
    fn navigation_geometry(&self) -> Result<NativeMarkdownPagedNavigationGeometry> {
        self.require()?;
        Ok(self.navigation.clone())
    }
    fn navigate(
        &self,
        action: Nav,
        extend: bool,
        context: &NativeMarkdownPagedNavigationContext,
    ) -> Result<NativeMarkdownPagedNavigationTarget> {
        self.require()?;
        if !context.geometry.same(&self.navigation) {
            return Err(Error::Revoked);
        }
        let before = self.selection()?;
        let head = before.head_utf16;
        let affinity = context.head_affinity.unwrap_or(Affinity::Downstream);
        let ordered = before.anchor_utf16.min(head)..before.anchor_utf16.max(head);
        let collapse = |backward: bool| if backward { ordered.start } else { ordered.end };
        match action {
            Nav::Left | Nav::Right if extend || ordered.is_empty() => {
                let visual = self.text_window()?.visual_neighbor(
                    head,
                    affinity,
                    context.bidi_side,
                    action == Nav::Right,
                    &|| self.current(),
                );
                self.require()?;
                let visual = match visual {
                    Ok(visual) => visual,
                    Err(crate::native_surface::paged_line_viewport::NativeMarkdownComposedViewportError::Pending) => return Err(Error::Pending),
                    Err(cause) => return Err(scene_error(cause)),
                }; // 缺真实邻行仍是原Producer的待准备范围，不当作BOF/EOF或猜一个整数邻居。
                Ok(NativeMarkdownPagedNavigationTarget {
                    selection: MarkdownTextSelection {
                        anchor_utf16: if extend {
                            before.anchor_utf16
                        } else {
                            visual.utf16
                        },
                        head_utf16: visual.utf16,
                    },
                    head_affinity: Some(visual.affinity),
                    vertical_goal: None,
                    bidi_side: Some(visual.side),
                })
            }
            Nav::Backward | Nav::Forward | Nav::Left | Nav::Right => {
                let backward = match action {
                    Nav::Left | Nav::Right => {
                        // 原CM非空选择只collapse一次；左右的from/to由真实段base决定，
                        // 不能用首part/最小level或mixed标记猜RTL段方向。
                        let base = match self.text_window()?.paragraph_base_level_at(head, affinity, &|| self.current()) {
                            Ok(base) => base,
                            Err(crate::native_surface::paged_line_viewport::NativeMarkdownComposedViewportError::Pending) => return Err(Error::Pending),
                            Err(cause) => return Err(scene_error(cause)),
                        };
                        (action == Nav::Left) == (base == 0)
                    }
                    _ => action == Nav::Backward, // 保留原标准逻辑Backward/Forward的既有语义。
                };
                let target = if !extend && !ordered.is_empty() {
                    collapse(backward)
                } else {
                    self.boundary(head, !backward)?
                };
                self.target(
                    target,
                    extend,
                    Some(if backward {
                        Affinity::Upstream
                    } else {
                        Affinity::Downstream
                    }),
                    None,
                )
            }
            Nav::LineStart | Nav::LineEnd | Nav::LineLeft | Nav::LineRight => {
                if matches!(action, Nav::LineLeft | Nav::LineRight) && !self.simple_ltr {
                    return Err(Error::UnsupportedCommand);
                }
                let line = self.line(head, affinity)?;
                let start = matches!(action, Nav::LineStart | Nav::LineLeft);
                let range = self.visible_line_range(line);
                self.target(
                    if start { range.start } else { range.end },
                    extend,
                    Some(if start {
                        Affinity::Downstream
                    } else {
                        Affinity::Upstream
                    }),
                    None,
                )
            }
            Nav::ParagraphStart | Nav::ParagraphEnd | Nav::SelectParagraph | Nav::SelectLine => {
                let line = self.line(head, affinity)?;
                let range = if action == Nav::SelectLine {
                    self.visible_line_range(line)
                } else {
                    self.paragraphs[line].clone()
                };
                if matches!(action, Nav::SelectParagraph | Nav::SelectLine) {
                    self.require()?;
                    Ok(NativeMarkdownPagedNavigationTarget {
                        selection: MarkdownTextSelection {
                            anchor_utf16: range.start,
                            head_utf16: range.end,
                        },
                        head_affinity: Some(Affinity::Upstream),
                        vertical_goal: None,
                        bidi_side: None,
                    })
                } else {
                    self.target(
                        if action == Nav::ParagraphStart {
                            range.start
                        } else {
                            range.end
                        },
                        extend,
                        Some(if action == Nav::ParagraphStart {
                            Affinity::Downstream
                        } else {
                            Affinity::Upstream
                        }),
                        None,
                    )
                }
            }
            Nav::DocumentStart | Nav::DocumentEnd => self.target(
                if action == Nav::DocumentStart {
                    0
                } else {
                    self.key.request.input.utf16_len
                },
                extend,
                Some(if action == Nav::DocumentStart {
                    Affinity::Downstream
                } else {
                    Affinity::Upstream
                }),
                None,
            ),
            Nav::Up | Nav::Down | Nav::PageUp | Nav::PageDown => {
                let caret = self
                    .text_window()?
                    .caret_geometry(head, affinity, &|| self.current())
                    .map_err(scene_error)?;
                if caret.secondary.is_some() {
                    return Err(Error::UnsupportedCommand);
                }
                let old_goal = context
                    .vertical_goal
                    .filter(|goal| self.navigation.permits_goal(*goal));
                let x = old_goal.map_or(caret.primary.origin.x, |goal| goal.container_x());
                let line = self.line(head, affinity)?;
                let backwards = matches!(action, Nav::Up | Nav::PageUp);
                let target_y = if matches!(action, Nav::PageUp | Nav::PageDown) {
                    caret.primary.origin.y
                        + if backwards {
                            self.key.request.viewport.size.height
                        } else {
                            -self.key.request.viewport.size.height
                        }
                } else {
                    let target_line = if backwards {
                        line.checked_sub(1)
                    } else {
                        line.checked_add(1)
                    }
                    .filter(|line| *line < self.ranges.len())
                    .ok_or_else(|| self.missing_geometry())?;
                    let frame = self
                        .text_window()?
                        .line_frame(target_line)
                        .ok_or(Error::InvalidGeometry)?;
                    frame.origin.y + frame.size.height * 0.5
                };
                let hit = self
                    .text_window()?
                    .hit(NSPoint::new(x, target_y), &|| self.current())
                    .map_err(scene_error)?;
                if hit.other_source_utf16.is_some() {
                    return Err(Error::UnsupportedCommand);
                }
                let goal = if matches!(action, Nav::Up | Nav::Down) {
                    old_goal.or_else(|| {
                        NativeMarkdownVerticalGoal::new(
                            caret.primary.origin.x,
                            caret.primary.origin.x,
                            self.navigation.width_key,
                            self.navigation.layout_epoch,
                        )
                    })
                } else {
                    None
                };
                let hit_index = hit
                    .line_ordinal
                    .checked_sub(self.first_ordinal)
                    .filter(|index| *index < self.ranges.len())
                    .ok_or(Error::InvalidGeometry)?;
                self.target(
                    hit.source_utf16,
                    extend,
                    Some(if hit.source_utf16 == self.ranges[hit_index].end {
                        Affinity::Upstream
                    } else {
                        Affinity::Downstream
                    }),
                    goal,
                )
            }
            Nav::WordBackward
            | Nav::WordForward
            | Nav::WordLeft
            | Nav::WordRight
            | Nav::SelectWord => Err(Error::UnsupportedCommand),
        }
    }
    fn edit_plan(&self, action: Edit) -> Result<NativeMarkdownPagedEditPlan> {
        self.require()?;
        let range = self
            .key
            .request
            .input
            .marked
            .unwrap_or(self.key.request.input.primary_range);
        let end = range
            .location
            .checked_add(range.length)
            .ok_or(Error::InvalidRange)?;
        if end > self.key.request.input.utf16_len {
            return Err(Error::InvalidRange);
        }
        let mut replacement = range;
        let inserted = match action {
            Edit::InsertNewline => "\n".to_owned(),
            Edit::InsertTab => "\t".to_owned(),
            Edit::DeleteBackward | Edit::DeleteForward => {
                if range.length == 0 {
                    let backward = action == Edit::DeleteBackward;
                    let target = self.boundary(range.location, !backward)?;
                    let start = target.min(range.location);
                    replacement = NSRange::new(start, target.max(range.location) - start);
                }
                String::new()
            }
            Edit::DeleteToLineStart | Edit::DeleteToLineEnd => {
                let line = self.line(range.location, Affinity::Downstream)?;
                let target = if action == Edit::DeleteToLineStart {
                    self.ranges[line].start
                } else {
                    self.visible_line_range(line).end
                };
                let start = range.location.min(target);
                replacement = NSRange::new(start, end.max(target) - start);
                String::new()
            }
            // 单词删除依真实 word producer，反向 Tab 依原缩进命令，不能假造为普通字节删除。
            Edit::DeleteWordBackward | Edit::DeleteWordForward | Edit::InsertBacktab => {
                return Err(Error::UnsupportedCommand);
            }
        };
        self.require()?;
        Ok(NativeMarkdownPagedEditPlan {
            replacement,
            inserted,
        })
    }
    fn begin_cpu_paint(
        self: Rc<Self>,
        context: CFRetained<CGContext>,
        current: &dyn Fn() -> bool,
    ) -> Result<Box<dyn NativeMarkdownPagedScenePaintSession>> {
        // 正文与Grid都只在构造时填固定画布；全部装饰沿同一量子预算继续，不能同步扫完整窗口。
        // 原同步paint保独立完整像素对照，不参与异步候选的背景、选区或字形推进。
        self.prepare_paint_space(current)?;
        self.fill_canvas(&context, &|| current() && self.current())?;
        Ok(Box::new(Paint::new(self, context)))
    }
    fn paint(&self, context: &CGContext, current: &dyn Fn() -> bool) -> Result<()> {
        self.prepare_paint_space(current)?;
        self.decorations(context, current)?;
        if let Some(window) = &self.window {
            let receipt = window
                .paint(context, &|| current() && self.current())
                .map_err(paint_error)?;
            if receipt.missing_glyphs != 0 {
                return Err(Error::UnsupportedCommand);
            }
        }
        for row in &self.grids {
            for cell in &row.cells {
                if let GridCellContent::Composed(window) = &cell.content {
                    let receipt = window
                        .paint(context, &|| current() && self.current())
                        .map_err(paint_error)?;
                    if receipt.missing_glyphs != 0 {
                        return Err(Error::UnsupportedCommand);
                    }
                }
            }
        }
        // 独立同步 paint 也消费原完整 Input 的组合外观；不另画字形或取得后继 marked 票。
        // 与正式 CPU 会话共用同一个逐 part 装饰实现，所有范围映射、字体度量及撤权规则一致。
        if let Some(marked) = self.key.request.input.marked {
            use crate::native_surface::paged_line_viewport::NativeMarkdownComposedViewportDecorationCursor;
            let marked = marked.location..marked.location + marked.length;
            let ranges = std::slice::from_ref(&marked);
            let permit = || current() && self.current();
            if let Some(window) = &self.window {
                let mut cursor = NativeMarkdownComposedViewportDecorationCursor::selections(
                    window.source_range(),
                    ranges,
                )
                .marked_text();
                while !window
                    .advance_decorations(
                        &mut cursor,
                        context,
                        ranges,
                        self.colors.text,
                        NativeMarkdownComposedViewportPaintBudget::default().metadata_records,
                        &permit,
                    )
                    .map_err(paint_error)?
                    .complete
                {}
            }
            // 格窗口仍先取其真实 canonical 域；不能把原 Body 的端点直接当 display 端点。
            // 同步入口原本逐个消费有限窗口，此处不进入未准备行或启动任何额外排版。
            for row in &self.grids {
                for cell in &row.cells {
                    if let GridCellContent::Composed(window) = &cell.content {
                        let mut cursor = window
                            .selection_cursor(ranges, &permit)
                            .map_err(scene_error)?
                            .marked_text();
                        while !window
                            .advance_decorations(
                                &mut cursor,
                                context,
                                ranges,
                                self.colors.text,
                                NativeMarkdownComposedViewportPaintBudget::default()
                                    .metadata_records,
                                &permit,
                            )
                            .map_err(paint_error)?
                            .complete
                        {}
                    }
                }
            }
        }
        self.caret(context, current)
    }
}

fn paint_error(cause: NativeMarkdownComposedViewportPaintError) -> Error {
    match cause.cause {
        NativeMarkdownComposedViewportPaintCause::Window(cause) => scene_error(cause),
        other => Error::Raster(other.to_string()),
    }
}
