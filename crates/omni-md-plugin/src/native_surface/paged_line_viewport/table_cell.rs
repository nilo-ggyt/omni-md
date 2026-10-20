//! 只读格消费真正的 display Source；所有对外位置映回 canonical，编辑仍由原唯一 cell session 承担。

use super::*;
use crate::native_surface::paged_text_view::NativeMarkdownPagedHeadAffinity;
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::CGContext;
use omni_markdown_core::MarkdownComposedLineAffinity;

/// 内层窗口不暴露 display 局部输入口，也不保活 Doc、Body、编辑器或另一个字体目录。
pub struct NativeMarkdownComposedTableCell {
    window: Rc<NativeMarkdownComposedViewport>,
}

impl NativeMarkdownComposedTableCell {
    /// 只读格拖选沿原实际显示行投影；canonical命中仍由下面同一display映射完成。
    pub(in crate::native_surface) fn project_drag_point(
        &self,
        local: NSPoint,
        current: &dyn Fn() -> bool,
    ) -> Result<Option<NSPoint>> {
        self.window.project_drag_point(local, current)
    }

    /// 真实列宽已由原表格解算器确定；content_frame 属同一行，裁剪只取它与外层视口的交集。
    ///
    /// # Errors
    /// 原 publication/场景、字体、Source 或实际行输出失效时拒绝，离屏格不创建占位窗口。
    pub fn prepare(
        output: Rc<RefCell<MarkdownComposedLines>>,
        key: NativeMarkdownPagedSceneKey,
        display: Rc<MarkdownTableCellDisplaySource>,
        palette: Vec<Rc<NativeMarkdownLineFont>>,
        handles: impl IntoIterator<Item = impl Into<NativeMarkdownMeasuredLine>>,
        content_frame: NSRect,
        current: &dyn Fn() -> bool,
    ) -> Result<Self> {
        // 显示格只嵌入真实正文 Preview；不能让 active draft 或 Source 模式借另一坐标域消费它。
        if !key.request.target.is_body(&key.request.document)
            || key.presentation_mode != MarkdownPresentationMode::InlinePreview
        {
            return Err(NativeMarkdownComposedViewportError::InvalidCoverage);
        }
        if !rect(content_frame) {
            return Err(NativeMarkdownComposedViewportError::InvalidGeometry);
        }
        let viewport = key.request.viewport;
        let left = viewport.origin.x.max(content_frame.origin.x);
        let bottom = viewport.origin.y.max(content_frame.origin.y);
        let right = (viewport.origin.x + viewport.size.width)
            .min(content_frame.origin.x + content_frame.size.width);
        let top = (viewport.origin.y + viewport.size.height)
            .min(content_frame.origin.y + content_frame.size.height);
        if right <= left || top <= bottom {
            return Err(NativeMarkdownComposedViewportError::Pending);
        } // 真正离屏的格留在原 spool，不能为它分配整格像素或签空 Ready。
        let alignment = {
            // 同原 Doc/root 核方向后归还租约；后继 Native getter 不跨此借用，旧格不能续权。
            let original = key
                .request
                .document
                .upgrade()
                .ok_or(NativeMarkdownComposedViewportError::Revoked)?;
            let document = original
                .try_borrow()
                .map_err(|_| NativeMarkdownComposedViewportError::Busy)?;
            display
                .canonical_raw_cell()
                .alignment_for(&document)
                .map_err(NativeMarkdownComposedViewportError::Table)?
        };
        let domain = LayoutDomain::TableCell {
            display,
            alignment,
            width: content_frame.size.width,
            clip: NSRect::new(
                NSPoint::new(left, bottom),
                NSSize::new(right - left, top - bottom),
            ),
            identity: Rc::new(()),
        };
        let first_top = NSPoint::new(
            content_frame.origin.x,
            content_frame.origin.y + content_frame.size.height,
        );
        let window = NativeMarkdownComposedViewport::prepare_domain(
            output, key, domain, palette, handles, first_top, current,
        )?;
        Ok(Self {
            window: Rc::new(window),
        })
    }

    /// 这里只借同一个封存显示 Source；不会从原文重新解析、创建第二份显示文本或补建字体。
    fn display(&self) -> &MarkdownTableCellDisplaySource {
        match &self.window.domain {
            LayoutDomain::TableCell { display, .. } => display,
            LayoutDomain::Body => {
                unreachable!("only the sealed table-cell constructor builds this owner")
            }
        }
    }

    /// 命中返回 canonical 两个真实候选；Bidi 另一侧不会因映射而被静默抹掉。
    pub fn hit(
        &self,
        point: NSPoint,
        current: &dyn Fn() -> bool,
    ) -> Result<NativeMarkdownComposedViewportHit> {
        let hit = self.window.hit(point, current)?;
        self.canonical_hit(hit, current)
    }

    /// 词组侧沿同一个显示窗口的真实GCB几何，再映回原raw格；分词不读取或清洗整格。
    pub(in crate::native_surface) fn word_hit(
        &self,
        point: NSPoint,
        current: &dyn Fn() -> bool,
    ) -> Result<(NativeMarkdownComposedViewportHit, bool)> {
        let (hit, backward_bias) = self.window.word_hit(point, current)?;
        Ok((self.canonical_hit(hit, current)?, backward_bias))
    }

    /// 普通命中和选词共享这一次真实display→canonical映射，主次候选各保其原part亲和。
    fn canonical_hit(
        &self,
        hit: NativeMarkdownComposedViewportHit,
        current: &dyn Fn() -> bool,
    ) -> Result<NativeMarkdownComposedViewportHit> {
        let source_utf16 = self.map_to_canonical(hit.source_utf16, hit.source_affinity)?;
        let other_source_utf16 = hit
            .other_source_utf16
            .zip(hit.other_source_affinity)
            .map(|(value, affinity)| self.map_to_canonical(value, affinity))
            .transpose()?;
        self.window.require(current)?;
        Ok(NativeMarkdownComposedViewportHit {
            source_utf16,
            source_affinity: hit.source_affinity,
            other_source_affinity: hit.other_source_affinity,
            other_source_utf16,
            line_ordinal: hit.line_ordinal,
        })
    }

    /// 激活仍交原 publication 查询真实链接目标；显示样式只授予当前可见墨迹的 canonical 范围。
    pub fn link_part_at(
        &self,
        point: NSPoint,
        current: &dyn Fn() -> bool,
    ) -> Result<Option<Range<usize>>> {
        let range = self.window.link_part_at(point, current)?;
        let mapped = range
            .map(|range| -> Result<Range<usize>> {
                Ok(
                    self.map_to_canonical(range.start, MarkdownComposedLineAffinity::Downstream)?
                        ..self
                            .map_to_canonical(range.end, MarkdownComposedLineAffinity::Upstream)?,
                )
            })
            .transpose()?;
        self.window.require(current)?;
        Ok(mapped)
    }

    /// Find/IME 使用原 canonical 位置与显式换行亲和；隐藏标记映射沿同一个磁盘目录。
    pub fn first_rect(
        &self,
        range: NSRange,
        affinity: NativeMarkdownPagedHeadAffinity,
        current: &dyn Fn() -> bool,
    ) -> Result<NativeMarkdownPagedCharacterRect> {
        self.window.require(current)?;
        let end = range
            .location
            .checked_add(range.length)
            .ok_or(NativeMarkdownComposedViewportError::InvalidRange)?;
        let start = self.map_to_display(range.location, affinity)?;
        let end = self.map_to_display(end, affinity)?;
        if end < start {
            return Err(NativeMarkdownComposedViewportError::InvalidRange);
        }
        let displayed_range = NSRange::new(start, end - start);
        let value = self
            .window
            .first_rect_with_affinity(displayed_range, affinity, current)?;
        if range.length == 0 && value.actual_range == displayed_range {
            // 隐藏强调/代码标记的两侧可映到同一个真实显示 caret。原 canonical 请求已经
            // 精确映到该零范围、底层也只返回它时，保留原端点；再次反映会按亲和越过隐藏标记，
            // 错把已完成选词的原端点改成闭标记之后，令后继可见性证明永久拒绝正确位置。
            self.window.require(current)?;
            return Ok(NativeMarkdownPagedCharacterRect {
                rect: value.rect,
                actual_range: range,
            });
        } // 非空请求或底层交付不同范围仍走真实反映，不能借此把隐藏字符/邻字形冒充原请求。
        let start = self.map_to_canonical(
            value.actual_range.location,
            if value.actual_range.length == 0 && affinity.is_upstream() {
                MarkdownComposedLineAffinity::Upstream
            } else {
                MarkdownComposedLineAffinity::Downstream
            },
        )?;
        let end = self.map_to_canonical(
            value.actual_range.location + value.actual_range.length,
            if value.actual_range.length == 0 && !affinity.is_upstream() {
                MarkdownComposedLineAffinity::Downstream
            } else {
                MarkdownComposedLineAffinity::Upstream
            },
        )?;
        self.window.require(current)?;
        Ok(NativeMarkdownPagedCharacterRect {
            rect: value.rect,
            actual_range: NSRange::new(start, end - start),
        })
    }

    /// 实际点击激活只借同一个原格票；行列、范围和 Root 由 Core 票管理，不复制另一套坐标。
    pub(in crate::native_surface) fn canonical_cell_source(
        &self,
    ) -> &omni_markdown_core::markdown_document_buffer::VerifiedMarkdownTableCellSource {
        self.display().canonical_raw_cell()
    }

    /// 模式切换只从真实可见字形取得原文锚点；不把隐藏表格分隔符或离屏行首当可见文字。
    pub(in crate::native_surface) fn first_visible_source(
        &self,
        current: &dyn Fn() -> bool,
    ) -> Result<Option<usize>> {
        self.window.require(current)?;
        let position = self
            .window
            .lines
            .iter()
            .filter(|line| self.window.visible(line.frame))
            .flat_map(|line| &line.parts)
            .find(|part| {
                part.handle.facts().kind == omni_markdown_core::MarkdownComposedLinePartKind::Glyphs
                    && !part.handle.facts().source_utf16.is_empty()
            })
            .map(|part| part.handle.facts().source_utf16.start);
        let canonical = position
            .map(|position| {
                self.map_to_canonical(position, MarkdownComposedLineAffinity::Downstream)
            })
            .transpose()?;
        self.window.require(current)?;
        Ok(canonical)
    }

    /// 实时查找只读取已封存且实际相交的软行；巨格离屏前缀不能借raw全格范围重新扫描。
    pub(in crate::native_surface) fn visible_canonical_range(
        &self,
        current: &dyn Fn() -> bool,
    ) -> Result<Option<Range<usize>>> {
        self.window.require(current)?;
        let mapped =
            self.visible_display_range()
                .map(|range| -> Result<Range<usize>> {
                    Ok(self
                        .map_to_canonical(range.start, MarkdownComposedLineAffinity::Downstream)?
                        ..self
                            .map_to_canonical(range.end, MarkdownComposedLineAffinity::Upstream)?)
                })
                .transpose()?;
        self.window.require(current)?;
        Ok(mapped.filter(|range| !range.is_empty()))
    }

    /// 查找消费真实显示目录的逐run片段，canonical包络仅作排序，不能把隐藏语法补回匹配域。
    /// 最大输入直接使用整个视口的剩余额度；返回借用迭代器，不先收集巨格全部来源记录。
    pub(in crate::native_surface) fn visible_find_ranges(
        &self,
        maximum_units: usize,
        current: &dyn Fn() -> bool,
    ) -> Result<Option<omni_markdown_core::MarkdownTableCellVisibleSourceRanges<'_>>> {
        self.window.require(current)?;
        let Some(range) = self.visible_display_range() else {
            return Ok(None);
        };
        let original = self
            .window
            .key
            .request
            .document
            .upgrade()
            .ok_or(NativeMarkdownComposedViewportError::Revoked)?;
        let document = original
            .try_borrow()
            .map_err(|_| NativeMarkdownComposedViewportError::Busy)?;
        let ranges = self
            .display()
            .visible_canonical_ranges_for(&document, range, maximum_units)
            .map_err(NativeMarkdownComposedViewportError::Table)?;
        drop(document);
        self.window.require(current)?;
        Ok(Some(ranges))
    }

    /// 真实可见软行共享一个display连续域；这里仅借已封存的行metadata，不读原文字节。
    fn visible_display_range(&self) -> Option<Range<usize>> {
        let mut range: Option<Range<usize>> = None;
        for line in self
            .window
            .lines
            .iter()
            .filter(|line| self.window.visible(line.frame))
        {
            let actual = line.handle.facts().source_utf16.clone();
            if actual.is_empty() {
                continue;
            }
            if let Some(range) = &mut range {
                range.end = actual.end;
            } else {
                range = Some(actual);
            }
        }
        range
    }

    /// 当前格的原 canonical 范围只从原 sealed 结构票取得；租约不跨 Native 或调用者回调。
    pub(in crate::native_surface) fn canonical_range(
        &self,
        current: &dyn Fn() -> bool,
    ) -> Result<Range<usize>> {
        self.window.require(current)?;
        let range = {
            let owner = self
                .window
                .key
                .request
                .document
                .upgrade()
                .ok_or(NativeMarkdownComposedViewportError::Revoked)?;
            let document = owner
                .try_borrow()
                .map_err(|_| NativeMarkdownComposedViewportError::Busy)?;
            self.canonical_cell_source()
                .canonical_range_for(&document)
                .map_err(NativeMarkdownComposedViewportError::Table)?
                .clone()
        };
        self.window.require(current)?;
        Ok(range)
    }

    /// 正文的跨格选择先与真实 raw 格相交，再映入 display；不把全篇范围交给局部输入口。
    pub fn selection_rects(
        &self,
        selection: Range<usize>,
        current: &dyn Fn() -> bool,
        visitor: impl FnMut(NSRect) -> Result<()>,
    ) -> Result<usize> {
        self.window.require(current)?;
        if selection.start > selection.end {
            return Err(NativeMarkdownComposedViewportError::InvalidRange);
        }
        let canonical = self.canonical_range(current)?;
        let start = selection.start.max(canonical.start);
        let end = selection.end.min(canonical.end);
        if start >= end {
            return Ok(0);
        }
        let start = self.map_to_display(start, NativeMarkdownPagedHeadAffinity::Downstream)?;
        let end = self.map_to_display(end, NativeMarkdownPagedHeadAffinity::Upstream)?;
        self.window.selection_rects(start..end, current, visitor)
    }

    /// 先读取真正的派生目录健康，再判断外层撤权；另一格的取消不能掩盖当前格已经发生的 I/O。
    pub(in crate::native_surface) fn source_health(&self) -> Result<()> {
        self.display()
            .source()
            .byte_offset_for_utf16(0)
            .map(|_| ())
            .map_err(|error| MarkdownLineComposeError::Source(error).into())
    }

    /// 可复用像素仍重核原 output、有限字体与当前 publication；没有正文重排或额外资源捕获。
    pub fn validate_ready(&self, current: &dyn Fn() -> bool) -> Result<()> {
        self.window.validate_ready(current)
    }

    /// 只建立单个实际格的代码底色游标；未调用 Native，不增加 context 或来源 owner。
    pub(in crate::native_surface) fn code_background_cursor(
        &self,
    ) -> NativeMarkdownComposedViewportDecorationCursor {
        NativeMarkdownComposedViewportDecorationCursor::backgrounds()
    }

    /// 选区端点先从原格取得；跨量子只保整数与原 Scene 的范围引用，不复制选中文本。
    pub(in crate::native_surface) fn selection_cursor(
        &self,
        selections: &[Range<usize>],
        current: &dyn Fn() -> bool,
    ) -> Result<NativeMarkdownComposedViewportDecorationCursor> {
        Ok(NativeMarkdownComposedViewportDecorationCursor::selections(
            self.canonical_range(current)?,
            selections,
        ))
    }

    /// 代码背景与选区共用外层本轮真实 metadata 余量，不能按每格重领一次完整预算。
    pub(in crate::native_surface) fn advance_decorations(
        &self,
        cursor: &mut NativeMarkdownComposedViewportDecorationCursor,
        context: &CGContext,
        selections: &[Range<usize>],
        selection_color: [f64; 4],
        budget: usize,
        current: &dyn Fn() -> bool,
    ) -> std::result::Result<
        NativeMarkdownComposedViewportDecorationStep,
        NativeMarkdownComposedViewportPaintError,
    > {
        self.window.advance_decorations(
            cursor,
            context,
            selections,
            selection_color,
            budget,
            current,
        )
    }

    /// 同一原窗口负责代码底色；图形上下文仅画真实格内的可见范围。
    pub fn paint_code_backgrounds(
        &self,
        context: &CGContext,
        current: &dyn Fn() -> bool,
    ) -> std::result::Result<(), NativeMarkdownComposedViewportPaintError> {
        self.window.paint_code_backgrounds(context, current)
    }

    /// 显式同步对照沿原总字形上限；正式显示继续由可续CPU会话负责，不截断可见前缀。
    pub(in crate::native_surface) fn paint(
        &self,
        context: &CGContext,
        current: &dyn Fn() -> bool,
    ) -> std::result::Result<
        NativeMarkdownComposedViewportPaintReceipt,
        NativeMarkdownComposedViewportPaintError,
    > {
        self.window.paint(context, current)
    }

    /// 仍沿原有界绘图游标；收据额外绑定这一个格窗口，不能冒充外层 Body 已绘完。
    pub fn begin_paint(
        &self,
        context: CFRetained<CGContext>,
        current: &dyn Fn() -> bool,
    ) -> std::result::Result<
        NativeMarkdownComposedViewportPaintSession,
        NativeMarkdownComposedViewportPaintError,
    > {
        self.window.clone().begin_paint(context, current)
    }

    /// 同一 outer key/context 的其他格或 Body 收据都不能领取当前格的完成事实。
    #[must_use]
    pub fn permits_completion(
        &self,
        completion: &NativeMarkdownComposedViewportPaintCompletion,
        context: &CGContext,
    ) -> bool {
        completion.permits_cell(&self.window, context)
    }

    /// 行高来自原实际字体/换行结果，供唯一表格行解算器累计，不按字符数估算。
    #[must_use]
    pub fn line_frame(&self, ordinal: usize) -> Option<NSRect> {
        self.window.line_frame(ordinal)
    }

    /// 真实目录读取错误保留为 Source 错误，不能变成普通离屏或撤权。
    fn map_to_canonical(
        &self,
        position: usize,
        affinity: MarkdownComposedLineAffinity,
    ) -> Result<usize> {
        self.display()
            .display_to_canonical(position, affinity)
            .map_err(|error| MarkdownLineComposeError::Source(error).into())
    }

    /// 范围外不是当前格；半代理和目录故障仍从同一原 display Source 明确返回。
    fn map_to_display(
        &self,
        position: usize,
        affinity: NativeMarkdownPagedHeadAffinity,
    ) -> Result<usize> {
        let affinity = if affinity.is_upstream() {
            MarkdownComposedLineAffinity::Upstream
        } else {
            MarkdownComposedLineAffinity::Downstream
        };
        self.display()
            .canonical_to_display(position, affinity)
            .map_err(|error| {
                NativeMarkdownComposedViewportError::Composed(MarkdownLineComposeError::Source(
                    error,
                ))
            })?
            .ok_or(NativeMarkdownComposedViewportError::InvalidRange)
    }
}
