//! 原已封存窗口的输入和绘图消费；任何方法都不创建新 Source 或推进排版。

use std::{
    cell::RefCell,
    ops::Range,
    rc::{Rc, Weak},
};

use objc2_core_foundation::CFRetained;
use objc2_core_graphics::CGContext;
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
        NativeMarkdownComposedViewport, NativeMarkdownComposedViewportPaintBudget,
        NativeMarkdownComposedViewportPaintCause, NativeMarkdownComposedViewportPaintError,
        NativeMarkdownComposedViewportPaintOutcome, NativeMarkdownComposedViewportPaintSession,
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

pub(in crate::native_surface) struct Scene {
    key: NativeMarkdownPagedSceneKey,
    _nonce: NativeMarkdownPagedSceneNonce,
    geometry: NativeMarkdownPagedViewportGeometry,
    window: Rc<NativeMarkdownComposedViewport>,
    output: Rc<RefCell<MarkdownComposedLines>>,
    owner: Weak<Owner>,
    ranges: Box<[Range<usize>]>,
    paragraphs: Box<[Range<usize>]>,
    simple_ltr: bool,
    navigation: NativeMarkdownPagedNavigationGeometry,
    colors: NativeMarkdownPagedBodyColors,
    head_affinity: Option<Affinity>,
    first_ordinal: usize,
    /// 仅保存发布时同一输入票的有限范围；高亮不能用主范围冒充全部选中。
    selections: Box<[Range<usize>]>,
    /// 仅原Source单有向选择的canonical逻辑行；UBA段和soft line均不授予此范围。
    active_line: Option<Range<usize>>,
    /// Source同次发布的有限行号；真实软行几何与canonical逻辑行只在Producer相交一次。
    gutter: Option<Box<[GutterRow]>>,
}

/// 一次准备结束的有限字段集合；构造范围仅限 Body，外界不能凭范围制造 Ready。
pub(super) struct Publication {
    pub key: NativeMarkdownPagedSceneKey,
    pub nonce: NativeMarkdownPagedSceneNonce,
    pub geometry: NativeMarkdownPagedViewportGeometry,
    pub window: Rc<NativeMarkdownComposedViewport>,
    pub output: Rc<RefCell<MarkdownComposedLines>>,
    pub owner: Weak<Owner>,
    pub ranges: Box<[Range<usize>]>,
    pub paragraphs: Box<[Range<usize>]>,
    pub simple_ltr: bool,
    pub width_key: i64,
    pub layout_epoch: u64,
    pub colors: NativeMarkdownPagedBodyColors,
    pub head_affinity: Option<Affinity>,
    pub first_ordinal: usize,
    pub selections: Box<[Range<usize>]>,
    pub active_line: Option<Range<usize>>,
    pub gutter: Option<Box<[GutterRow]>>,
}

impl Scene {
    pub(super) fn new(publication: Publication) -> Self {
        let Publication {
            key,
            nonce,
            geometry,
            window,
            output,
            owner,
            ranges,
            paragraphs,
            simple_ltr,
            width_key,
            layout_epoch,
            colors,
            head_affinity,
            first_ordinal,
            selections,
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
            owner,
            ranges,
            paragraphs,
            simple_ltr,
            navigation,
            colors,
            head_affinity,
            first_ordinal,
            selections,
            active_line,
            gutter,
        }
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
            .window
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
            .window
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
            .window
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

    fn require(&self) -> Result<()> {
        self.window
            .validate_ready(&|| self.current())
            .map_err(scene_error)
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

    fn boundary(&self, position: usize, next: bool) -> Result<usize> {
        self.require()?;
        let answer = {
            let mut output = self.output.try_borrow_mut().map_err(|_| Error::Busy)?;
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

    fn decorations(&self, context: &CGContext, current: &dyn Fn() -> bool) -> Result<()> {
        self.validate_ready(current)?;
        color(context, self.colors.background);
        CGContext::fill_rect(Some(context), self.key.request.viewport);
        self.validate_ready(current)?;
        if let Some(active) = &self.active_line {
            color(context, self.colors.active_line);
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
                    .window
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
        let prepared = self.window.source_range();
        // 输入所有者已经认证范围有序且不重叠；二分跳过视口前的范围，不扫描其正文。
        // 这里只借发布时的整数元数据，离屏匹配不会推进排版或使匹配包络进入内存。
        let first = self
            .selections
            .partition_point(|selection| selection.end <= prepared.start);
        color(context, self.colors.selection);
        for selection in &self.selections[first..] {
            if selection.start >= prepared.end {
                break; // 后续范围同样位于本次准备窗口之后，没有本帧绘图后果。
            }
            if selection.is_empty() {
                continue; // 空范围是光标位置，不能制造额外的选中背景。
            }
            // 所有范围共用同一封存窗口和候选画布；任一真实几何或权限错误使整帧失败。
            self.window
                .selection_rects(selection.clone(), current, |rect| {
                    CGContext::fill_rect(Some(context), rect);
                    Ok(())
                })
                .map_err(scene_error)?;
        }
        color(context, self.colors.text);
        self.validate_ready(current)
    }

    fn caret(&self, context: &CGContext, current: &dyn Fn() -> bool) -> Result<()> {
        self.validate_ready(current)?;
        let input = self.key.request.input;
        if input.selection_range_count != 1 || input.primary_range.length != 0 {
            return Ok(());
        }
        let selection = self.selection()?;
        let prepared = self.window.source_range();
        if selection.head_utf16 < prepared.start || selection.head_utf16 > prepared.end {
            return Ok(()); // 离屏 caret 没有本次绘图后果，不为它重排或读取其他段落。
        }
        let geometry = self
            .window
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
        color(context, self.colors.caret);
        CGContext::fill_rect(Some(context), rect);
        self.validate_ready(current)
    }
}

impl NativeMarkdownPagedTextScene for Scene {
    fn key(&self) -> &NativeMarkdownPagedSceneKey {
        &self.key
    }
    fn cpu_frame_published(&self, paints: usize) {
        // CPU owner 已真正提交完整帧；这里只排同一个原 key 的弱通知，不在绘图栈调用外方。
        if let Some(owner) = self.owner.upgrade() {
            owner.queue_cpu_publication(&self.key, paints);
        }
    }
    fn validate_ready(&self, current: &dyn Fn() -> bool) -> Result<()> {
        self.window
            .validate_ready(&|| current() && self.current())
            .map_err(scene_error)
    }
    fn first_rect(&self, range: NSRange) -> Result<NativeMarkdownPagedCharacterRect> {
        let affinity =
            if range.length == 0 && range.location == self.key.request.input.selection.head_utf16 {
                self.head_affinity.unwrap_or(Affinity::Downstream)
            } else {
                Affinity::Downstream
            };
        self.window
            .first_rect_with_caret_side(range, affinity, self.bidi_side()?, &|| self.current())
            .map_err(scene_error)
    }
    fn hit(&self, local: NSPoint) -> Result<usize> {
        let hit = self
            .window
            .hit(local, &|| self.current())
            .map_err(scene_error)?;
        if hit.other_source_utf16.is_some() {
            return Err(Error::UnsupportedCommand);
        }
        Ok(hit.source_utf16)
    }
    fn word_hit(&self, local: NSPoint) -> Result<(usize, bool)> {
        let hit = self
            .window
            .hit(local, &|| self.current())
            .map_err(scene_error)?;
        if hit.other_source_utf16.is_some() {
            return Err(Error::UnsupportedCommand); // 真实同点的两个逻辑位置尚无主侧，不能猜词组来源。
        }
        let index = hit
            .line_ordinal
            .checked_sub(self.first_ordinal)
            .ok_or(Error::InvalidGeometry)?;
        let range = self.ranges.get(index).ok_or(Error::InvalidGeometry)?;
        let affinity = if hit.source_utf16 == range.end {
            Affinity::Upstream
        } else {
            Affinity::Downstream
        };
        let caret = self
            .window
            .caret_geometry(hit.source_utf16, affinity, &|| self.current())
            .map_err(scene_error)?;
        if caret.line_ordinal != hit.line_ordinal || caret.secondary.is_some() {
            return Err(Error::Pending); // soft-wrap侧与Bidi次插入点必须来自本次命中行，不能借邻行坐标。
        }
        for next in [true, false] {
            let position = self.boundary(hit.source_utf16, next)?;
            if position == hit.source_utf16 || position < range.start || position > range.end {
                continue; // 只比较当前真实可见行相邻GCB，硬换行/窗口外位置没有本次点击几何。
            }
            let adjacent = self
                .window
                .caret_geometry(
                    position,
                    if next {
                        Affinity::Upstream
                    } else {
                        Affinity::Downstream
                    },
                    &|| self.current(),
                )
                .map_err(scene_error)?;
            if adjacent.line_ordinal != hit.line_ordinal || adjacent.secondary.is_some() {
                continue; // 一个邻侧歧义时仍可由另一个实际同run邻侧证明方向，不丢弃真实错误。
            }
            let forward_x = if next {
                adjacent.primary.origin.x - caret.primary.origin.x
            } else {
                caret.primary.origin.x - adjacent.primary.origin.x
            };
            if forward_x == 0.0 {
                continue; // 零advance没有方向证明，不能由UTF16 ordinal或固定LTR默认值代替。
            }
            let backward_bias = if forward_x > 0.0 {
                local.x < caret.primary.origin.x
            } else {
                local.x > caret.primary.origin.x
            };
            self.require()?;
            return Ok((hit.source_utf16, backward_bias));
        }
        if range.is_empty() {
            self.require()?;
            return Ok((hit.source_utf16, false)); // 真空行无可选GCB；Core将核硬行并返回原空范围，bias不选择邻行。
        }
        Err(Error::Pending) // 尚无非零相邻字形方向时保真实未完成，不伪造选词侧。
    }
    fn mouse_selection(
        &self,
        local: NSPoint,
        clicks: usize,
        anchor: Option<usize>,
        extend: bool,
    ) -> Result<MarkdownTextSelection> {
        let position = self.hit(local)?;
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
            3 => {
                let line = self.line(position, Affinity::Downstream)?;
                let range = &self.paragraphs[line];
                MarkdownTextSelection {
                    anchor_utf16: range.start,
                    head_utf16: range.end,
                }
            }
            // 双击分词需要真实 word producer；不按 ASCII 空格猜中文和标点的选择范围。
            _ => return Err(Error::UnsupportedCommand),
        };
        self.require()?;
        Ok(selection)
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
                let visual = self.window.visual_neighbor(
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
                        let base = match self.window.paragraph_base_level_at(head, affinity, &|| self.current()) {
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
                let range = &self.ranges[line];
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
                    &self.ranges[line]
                } else {
                    &self.paragraphs[line]
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
                    .window
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
                        .window
                        .line_frame(target_line)
                        .ok_or(Error::InvalidGeometry)?;
                    frame.origin.y + frame.size.height * 0.5
                };
                let hit = self
                    .window
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
                    self.ranges[line].end
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
        self.decorations(&context, &|| current() && self.current())?;
        let session = self
            .window
            .clone()
            .begin_paint(context.clone(), &|| current() && self.current())
            .map_err(paint_error)?;
        Ok(Box::new(Paint {
            scene: self,
            context,
            session,
        }))
    }
    fn paint(&self, context: &CGContext, current: &dyn Fn() -> bool) -> Result<()> {
        self.decorations(context, current)?;
        let receipt = self
            .window
            .paint(context, &|| current() && self.current())
            .map_err(|cause| Error::Raster(cause.to_string()))?;
        if receipt.missing_glyphs != 0 {
            return Err(Error::UnsupportedCommand);
        }
        self.caret(context, current)
    }
}

struct Paint {
    scene: Rc<Scene>,
    context: CFRetained<CGContext>,
    session: NativeMarkdownComposedViewportPaintSession,
}
impl NativeMarkdownPagedScenePaintSession for Paint {
    fn step(
        self: Box<Self>,
        budget: NativeMarkdownPagedScenePaintBudget,
        current: &dyn Fn() -> bool,
    ) -> Result<NativeMarkdownPagedScenePaintStep> {
        let Self {
            scene,
            context,
            session,
        } = *self;
        let valid = || current() && scene.current();
        let step = session.step(
            NativeMarkdownComposedViewportPaintBudget {
                glyphs: budget.glyphs,
                metadata_records: budget.metadata_records,
            },
            &valid,
        );
        scene.validate_ready(&valid)?; // 原 Source 首错不能被绘图取消或 Native bool 掩盖。
        let step = step.map_err(paint_error)?;
        let work = NativeMarkdownPagedScenePaintWork {
            glyphs: step.work.glyphs,
            batches: step.work.batches,
            metadata_records: step.work.metadata_records,
        };
        let outcome = match step.outcome {
            NativeMarkdownComposedViewportPaintOutcome::Pending(session) => {
                NativeMarkdownPagedScenePaintOutcome::Pending(Box::new(Self {
                    scene,
                    context,
                    session,
                }))
            }
            NativeMarkdownComposedViewportPaintOutcome::Complete(completion) => {
                if completion.receipt().missing_glyphs != 0 {
                    return Err(Error::UnsupportedCommand);
                }
                scene.caret(&context, &valid)?;
                NativeMarkdownPagedScenePaintOutcome::Complete(
                    NativeMarkdownPagedScenePaintCompletion::from_composed(
                        completion, &scene.key, &context,
                    )?,
                )
            }
        };
        Ok(NativeMarkdownPagedScenePaintStep { work, outcome })
    }
}

fn color(context: &CGContext, color: [f64; 4]) {
    CGContext::set_rgb_fill_color(Some(context), color[0], color[1], color[2], color[3]);
}
fn paint_error(cause: NativeMarkdownComposedViewportPaintError) -> Error {
    match cause.cause {
        NativeMarkdownComposedViewportPaintCause::Window(cause) => scene_error(cause),
        other => Error::Raster(other.to_string()),
    }
}
