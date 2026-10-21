//! 有限可见行的真实GCB几何；不按UTF16平均分配，不保留整行字素列表。

use super::*;
use crate::native_surface::paged_line_geometry::{
    ComposedPartSourceEdge, composed_part_caret, composed_part_extent_edge,
};
use omni_markdown_core::{
    MarkdownComposedLinePartKind, MarkdownGraphemeAnswer, MarkdownGraphemeQuery,
    MarkdownPresentationStyles,
};

/// 一次命中最多查询固定数量的真实边界；超大零advance行由后继producer另行索引。
const HIT_BOUNDARIES: usize = 4_096;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NativeMarkdownComposedViewportHit {
    pub source_utf16: usize,
    /// 实际命中 part 的起/末侧；显示格删除语法后，相同局部 caret 两侧的 canonical 端点可以不同。
    pub source_affinity: omni_markdown_core::MarkdownComposedLineAffinity,
    /// 第二候选保它自己的实际 part 侧，不能借主候选的亲和解释 BiDi 边界。
    pub other_source_affinity: Option<omni_markdown_core::MarkdownComposedLineAffinity>,
    /// 同一实际视觉x对应两个逻辑位置时保留另一候选，外层不能据此伪造主方向。
    pub other_source_utf16: Option<usize>,
    pub line_ordinal: usize,
}

impl NativeMarkdownComposedViewport {
    /// 只线性枚举当前可见标签的真实推进包络，不查询目标或复制字形/正文。
    /// 原同View坐标与viewport/cell clip在本层确定，AppKit光标区域的排序合并归安装者负责。
    pub(in crate::native_surface) fn visit_link_cursor_rects(
        &self,
        current: &dyn Fn() -> bool,
        visit: &mut dyn FnMut(NSRect),
    ) -> Result<()> {
        self.require(current)?;
        for line in self.lines.iter().filter(|line| self.visible(line.frame)) {
            for part in &line.parts {
                let Some(area) = link_part_rect(line, part) else {
                    continue;
                };
                let area = objc2_foundation::NSIntersectionRect(area, self.clip());
                if area.size.width <= 0.0 || area.size.height <= 0.0 {
                    continue; // 离屏与零advance不扩大成整行或相邻格的悬停范围。
                }
                if !rect(area) {
                    return Err(NativeMarkdownComposedViewportError::InvalidGeometry);
                }
                self.require(current)?; // 只交纯Copy几何；调用者收集完毕并归还Scene后才可进入Native。
                visit(area);
                self.require(current)?; // Source首错仍来自原输出，临时集合不能给后继Scene补票。
            }
        }
        self.require(current)
    }

    /// Find只借当前可见软行的真实part源域；Preview的Markdown语法没有高亮权限，
    /// Source保留字面字符。折叠区和表格占位没有普通正文glyph，不能借行包络扫描其隐藏源。
    pub(in crate::native_surface) fn visible_find_ranges(
        &self,
        current: &dyn Fn() -> bool,
    ) -> Result<Vec<Range<usize>>> {
        self.require(current)?;
        let preview = self.key.presentation_mode == MarkdownPresentationMode::InlinePreview;
        let mut ranges = Vec::new();
        for line in self.lines.iter().filter(|line| self.visible(line.frame)) {
            for part in &line.parts {
                let facts = part.handle.facts();
                if facts.source_utf16.is_empty()
                    || matches!(
                        facts.kind,
                        MarkdownComposedLinePartKind::FoldedSection
                            | MarkdownComposedLinePartKind::TableRow
                    )
                    || preview
                        && (facts
                            .presentation
                            .contains(MarkdownPresentationStyles::MARKER)
                            || facts
                                .presentation
                                .contains(MarkdownPresentationStyles::HIDDEN_MARKER))
                {
                    continue;
                }
                ranges.push(facts.source_utf16.clone()); // 仅封存metadata，不读取、复制或解析正文。
            }
        }
        self.require(current)?;
        Ok(ranges)
    }

    /// 只给实际链接墨迹区域开放后继目标查询；样式位不能自行构造URL或扩大到行尾空白。
    pub fn link_part_at(
        &self,
        local: NSPoint,
        current: &dyn Fn() -> bool,
    ) -> Result<Option<Range<usize>>> {
        self.require(current)?;
        if !inside(local, self.clip()) {
            return Ok(None);
        }
        for line in &self.lines {
            if !self.visible(line.frame) {
                continue;
            }
            for part in &line.parts {
                let facts = part.handle.facts();
                let Some(area) = link_part_rect(line, part) else {
                    continue;
                };
                if inside(local, area) {
                    self.require(current)?;
                    return Ok(Some(facts.source_utf16.clone()));
                }
            }
        }
        Ok(None)
    }

    /// 正式IME/可见性与绘制消费同一选择槽的真实Bidi侧；非空范围仍定位首GCB。
    pub fn first_rect_with_caret_side(
        &self,
        range: NSRange,
        affinity: crate::native_surface::paged_text_view::NativeMarkdownPagedHeadAffinity,
        side: Option<crate::native_surface::paged_input::NativeMarkdownBidiCaretSide>,
        current: &dyn Fn() -> bool,
    ) -> Result<NativeMarkdownPagedCharacterRect> {
        if range.length != 0 {
            return self.first_rect_with_affinity(range, affinity, current);
        } // 真实字符框不由caret侧改写；只对同位置的两个插入点作明确选择。
        let caret = self.caret_geometry_with_side(range.location, affinity, side, current)?;
        if !self.visible(caret.line_frame) {
            return Err(NativeMarkdownComposedViewportError::Pending);
        }
        self.require(current)?;
        Ok(NativeMarkdownPagedCharacterRect {
            rect: caret.primary,
            actual_range: range,
        })
    }

    /// 首scalar所在GCB的实际typographic框；没有该首位置的可见行就保持Pending。
    ///
    /// # Errors
    /// 离屏、未知范围、原Source/Font/scene失权以及非GCB caret均拒绝，不取后继可见文字替代。
    pub fn first_rect(
        &self,
        range: NSRange,
        current: &dyn Fn() -> bool,
    ) -> Result<NativeMarkdownPagedCharacterRect> {
        self.first_rect_with_affinity(
            range,
            crate::native_surface::paged_text_view::NativeMarkdownPagedHeadAffinity::Downstream,
            current,
        )
    }

    /// IME/导航使用原选择槽的换行侧；不以同位置的后一行替换用户实际的上游光标。
    /// 非空范围仍定位首scalar所属GCB；BiDi双位置必须由更外层原选择证据解决。
    ///
    /// # Errors
    /// 与first_rect相同；零范围有两个不同视觉位置时返回Pending，不猜测主插入点。
    pub fn first_rect_with_affinity(
        &self,
        range: NSRange,
        affinity: crate::native_surface::paged_text_view::NativeMarkdownPagedHeadAffinity,
        current: &dyn Fn() -> bool,
    ) -> Result<NativeMarkdownPagedCharacterRect> {
        self.require(current)?;
        range
            .location
            .checked_add(range.length)
            .filter(|end| *end <= self.utf16_len())
            .ok_or(NativeMarkdownComposedViewportError::InvalidRange)?;
        if range.length == 0 {
            let caret = self.caret_geometry(range.location, affinity, current)?;
            if !self.visible(caret.line_frame) || caret.secondary.is_some() {
                return Err(NativeMarkdownComposedViewportError::Pending);
            } // 只读实际准备侧；换行侧不授予选择BiDi主/次位置的权限。
            self.require(current)?;
            return Ok(NativeMarkdownPagedCharacterRect {
                rect: caret.primary,
                actual_range: range,
            });
        }
        let (line, _) = self.part_for_position(range.location)?;
        if !self.visible(line.frame) {
            return Err(NativeMarkdownComposedViewportError::Pending);
        }
        // 非空请求扩到首scalar所属完整GCB；不把后继行或UTF16中点当作字形首位置。
        let start = self.boundary(range.location, false)?;
        let next = self.next_boundary(start)?;
        if next > line.handle.facts().source_utf16.end || next <= start {
            return Err(NativeMarkdownComposedViewportError::InvalidRange);
        }
        let actual = start..next;
        let mut rect: Option<NSRect> = None;
        let mut covered = 0usize;
        for part in &line.parts {
            let source = &part.handle.facts().source_utf16;
            let overlap = actual.start.max(source.start)..actual.end.min(source.end);
            if overlap.start >= overlap.end {
                continue;
            }
            // 同一 GCB 的字形可以属于数个真实 UBA/字体项；只聚合原有限行的实际包络。
            // 内部 seam 仅作绘制截面，actual_range 始终保留原完整 GCB，不给 IME 半个字符框。
            let next = self.part_rect(line, part, overlap.clone(), current)?;
            covered = covered
                .checked_add(overlap.len())
                .ok_or(NativeMarkdownComposedViewportError::InvalidRange)?;
            rect = Some(if let Some(previous) = rect {
                let left = previous.origin.x.min(next.origin.x);
                let bottom = previous.origin.y.min(next.origin.y);
                let right =
                    (previous.origin.x + previous.size.width).max(next.origin.x + next.size.width);
                let top = (previous.origin.y + previous.size.height)
                    .max(next.origin.y + next.size.height);
                NSRect::new(
                    NSPoint::new(left, bottom),
                    NSSize::new(right - left, top - bottom),
                )
            } else {
                next
            });
        }
        if covered != actual.len() {
            return Err(NativeMarkdownComposedViewportError::Pending);
        } // 未准备完整原 GCB 时不能把一个已显示片段签为整个字符。
        let rect = rect.ok_or(NativeMarkdownComposedViewportError::Pending)?;
        if !self.visible(rect) && rect.size.width != 0.0 {
            return Err(NativeMarkdownComposedViewportError::Pending);
        }
        self.require(current)?;
        Ok(NativeMarkdownPagedCharacterRect {
            rect,
            actual_range: NSRange::new(actual.start, actual.end - actual.start),
        })
    }

    /// 词组点击侧只由本次真实字形与相邻GCB的caret方向决定；Body和只读格共用此事实。
    /// 返回命中而非词范围，真正中文、标点和双向文字分词仍交同Source的原UBA生产者。
    pub(in crate::native_surface) fn word_hit(
        &self,
        local: NSPoint,
        current: &dyn Fn() -> bool,
    ) -> Result<(NativeMarkdownComposedViewportHit, bool)> {
        let hit = self.hit(local, current)?;
        if hit.other_source_utf16.is_some() {
            return Err(NativeMarkdownComposedViewportError::Pending);
        } // 同视觉位置仍有两个逻辑候选时，不能从UTF16大小或默认LTR猜词组侧。
        let line = self
            .lines
            .iter()
            .find(|line| line.handle.facts().ordinal == hit.line_ordinal)
            .ok_or(NativeMarkdownComposedViewportError::InvalidGeometry)?;
        let range = &line.handle.facts().source_utf16;
        let affinity =
            crate::native_surface::paged_text_view::NativeMarkdownPagedHeadAffinity::from_upstream(
                hit.source_affinity == omni_markdown_core::MarkdownComposedLineAffinity::Upstream,
            );
        let caret = self.caret_geometry(hit.source_utf16, affinity, current)?;
        if caret.line_ordinal != hit.line_ordinal || caret.secondary.is_some() {
            return Err(NativeMarkdownComposedViewportError::Pending);
        }
        for next in [true, false] {
            let position = {
                // 只读原封存GCB目录；Reader租约在Native几何或current回调前结束。
                let mut output = self
                    .output
                    .try_borrow_mut()
                    .map_err(|_| NativeMarkdownComposedViewportError::Busy)?;
                match output.grapheme_query(
                    hit.source_utf16,
                    if next {
                        MarkdownGraphemeQuery::Next
                    } else {
                        MarkdownGraphemeQuery::Previous
                    },
                )? {
                    MarkdownGraphemeAnswer::Known(Some(boundary)) => Some(boundary.utf16),
                    _ => None,
                }
            };
            self.require(current)?;
            let Some(position) =
                position.filter(|position| range.contains(position) || *position == range.end)
            else {
                continue; // 窗口外、硬换行或尚无相邻边界均不授方向，仍可检查另一真实同run邻侧。
            };
            let adjacent = self.caret_geometry(
                position,
                crate::native_surface::paged_text_view::NativeMarkdownPagedHeadAffinity::from_upstream(next),
                current,
            )?;
            if adjacent.line_ordinal != hit.line_ordinal || adjacent.secondary.is_some() {
                continue;
            }
            let forward_x = if next {
                adjacent.primary.origin.x - caret.primary.origin.x
            } else {
                caret.primary.origin.x - adjacent.primary.origin.x
            };
            if forward_x == 0.0 {
                continue; // 零advance没有可证明方向，不能把UTF16邻接当作真实视觉位置。
            }
            let backward_bias = if forward_x > 0.0 {
                local.x < caret.primary.origin.x
            } else {
                local.x > caret.primary.origin.x
            };
            self.require(current)?;
            return Ok((hit, backward_bias));
        }
        self.require(current)?;
        if range.is_empty() {
            return Ok((hit, false)); // 真空行没有词组，原UBA将返回同一空范围，不借相邻行补内容。
        }
        Err(NativeMarkdownComposedViewportError::Pending)
    }

    /// 有界顺序比较原GCB的实际caret，保留BiDi同视觉位置的两个逻辑候选。
    ///
    /// # Errors
    /// 点在窗口外、候选行尚未准备、边界预算不足或原权限失效时返回Pending/错误。
    /// 拖选终点只投到本窗口真正可见的封存行；保留原物理点供滚动使用，不猜离屏文字位置。
    pub(in crate::native_surface) fn project_drag_point(
        &self,
        local: NSPoint,
        current: &dyn Fn() -> bool,
    ) -> Result<Option<NSPoint>> {
        self.require(current)?;
        if !point(local) {
            return Err(NativeMarkdownComposedViewportError::InvalidGeometry);
        }
        let clip = self.clip();
        let mut nearest = None;
        let mut distance = f64::INFINITY;
        for line in &self.lines {
            if line.handle.facts().kind == omni_markdown_core::MarkdownComposedLineKind::TableRow
                || line.parts.is_empty()
                || line.parts.iter().all(|part| {
                    matches!(
                        part.handle.facts().kind,
                        MarkdownComposedLinePartKind::FoldedSection
                            | MarkdownComposedLinePartKind::TableRow
                    )
                })
            {
                continue; // 结构行交真实Grid；折叠隐区没有可用于拖选的可见墨迹或虚构行高。
            }
            if self.key.presentation_mode == MarkdownPresentationMode::InlinePreview {
                let mut hidden_text = false;
                let mut visible_text = false;
                for part in &line.parts {
                    let facts = part.handle.facts();
                    if matches!(
                        facts.kind,
                        MarkdownComposedLinePartKind::Glyphs | MarkdownComposedLinePartKind::Tab
                    ) {
                        if facts
                            .presentation
                            .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
                        {
                            hidden_text = true;
                        } else {
                            visible_text = true;
                        }
                    }
                }
                if hidden_text && !visible_text {
                    continue; // 整行只有0.1pt隐藏语法时没有拖选目标；真实空行的ParagraphBreak仍保留。
                }
            }
            let bottom = line.frame.origin.y.max(clip.origin.y);
            let top = (line.frame.origin.y + line.frame.size.height)
                .min(clip.origin.y + clip.size.height);
            if top <= bottom {
                continue;
            }
            // 严格小于右/上边，沿原hit的半开行框；next_down只选同一真实框内的最近可表示点。
            let projected = NSPoint::new(
                local
                    .x
                    .clamp(clip.origin.x, (clip.origin.x + clip.size.width).next_down()),
                local.y.clamp(bottom, top.next_down()),
            );
            let delta = (local.x - projected.x).hypot(local.y - projected.y);
            if delta < distance {
                nearest = Some(projected);
                distance = delta;
            }
        }
        self.require(current)?;
        Ok(nearest) // 仅遍历当前已拥有的有限行metadata；不读Source、GCB或建立新目录。
    }

    pub fn hit(
        &self,
        local: NSPoint,
        current: &dyn Fn() -> bool,
    ) -> Result<NativeMarkdownComposedViewportHit> {
        self.require(current)?;
        if !point(local) || !inside(local, self.clip()) {
            return Err(NativeMarkdownComposedViewportError::Pending);
        }
        let line = self
            .lines
            .iter()
            .find(|line| {
                local.y >= line.frame.origin.y
                    && local.y < line.frame.origin.y + line.frame.size.height
            })
            .ok_or(NativeMarkdownComposedViewportError::Pending)?;
        if line.handle.facts().kind == omni_markdown_core::MarkdownComposedLineKind::TableRow {
            return Err(NativeMarkdownComposedViewportError::Pending);
        }
        // 先以原目录rank预检全行查询量；没有预算时不能用已比较的前缀冒称最近位置。
        let mut count = 0usize;
        {
            for part in &line.parts {
                let Some(range) = self.part_grapheme_range(part)? else {
                    continue; // 两个整形 seam 之间可以没有可编辑边界，不把零个 GCB 当成缺源错误。
                };
                let mut reader = self
                    .output
                    .try_borrow_mut()
                    .map_err(|_| NativeMarkdownComposedViewportError::Busy)?;
                let start = reader
                    .grapheme_ordinal(*range.start())?
                    .ok_or(NativeMarkdownComposedViewportError::InvalidRange)?;
                let end = reader
                    .grapheme_ordinal(*range.end())?
                    .ok_or(NativeMarkdownComposedViewportError::InvalidRange)?;
                count = count
                    .checked_add(
                        end.checked_sub(start)
                            .and_then(|value| value.checked_add(1))
                            .ok_or(NativeMarkdownComposedViewportError::InvalidRange)?,
                    )
                    .ok_or(NativeMarkdownComposedViewportError::Pending)?;
                if count > HIT_BOUNDARIES {
                    return Err(NativeMarkdownComposedViewportError::Pending);
                }
            }
        }
        let mut answer: Option<NativeMarkdownComposedViewportHit> = None;
        let mut distance = f64::INFINITY;
        // 只在answer已由真实caret产生后参与比较；普通字符中点两侧同距不等于视觉位置重合。
        // 这个坐标属于本次有限扫描，不保存Source、后继输入或跨调用几何缓存。
        let mut primary_caret_x = 0.0;
        let mut primary_inside_part = false;
        // 有限GCB逐项比较真实caret；不假设连字内部GDEF单调，也不按源字节长度插值。
        for part in &line.parts {
            let Some(range) = self.part_grapheme_range(part)? else {
                continue;
            };
            let mut position = *range.start();
            loop {
                let caret = composed_part_caret(
                    &self.output,
                    &part.handle,
                    &self.palette[part.font],
                    position,
                    line.baseline,
                    &|| self.require(current).is_ok(),
                );
                self.require(current)?; // 原Source故障优先，不能被几何回调的bool撤权掩盖。
                let caret = caret?;
                let affinity = if position == part.handle.facts().source_utf16.end {
                    omni_markdown_core::MarkdownComposedLineAffinity::Upstream
                } else {
                    omni_markdown_core::MarkdownComposedLineAffinity::Downstream
                }; // 只消费原真实 part 边界，不按文字方向或平均 advance 猜测隐藏语法的来源侧。
                let facts = part.handle.facts();
                let part_start = line.baseline.x + facts.origin_x_26_6 as f64 / 64.0;
                let part_end = part_start + facts.x_advance_26_6 as f64 / 64.0;
                let inside_part =
                    local.x >= part_start.min(part_end) && local.x < part_start.max(part_end);
                let delta = (caret.baseline.x - local.x).abs();
                if delta < distance {
                    // 更近的视觉位置整体替换旧候选，旧BiDi歧义不能污染新结果。
                    distance = delta;
                    primary_caret_x = caret.baseline.x;
                    primary_inside_part = inside_part;
                    answer = Some(NativeMarkdownComposedViewportHit {
                        source_utf16: position,
                        source_affinity: affinity,
                        other_source_affinity: None,
                        other_source_utf16: None,
                        line_ordinal: line.handle.facts().ordinal,
                    });
                } else if delta == distance
                    && let Some(answer) = &mut answer
                    && answer.source_utf16 == position
                    && inside_part
                    && !primary_inside_part
                {
                    // 同源caret有两侧时，取点击点实际落入的墨迹跨度一侧，并同步真实视觉位置。
                    // 若主位置已从另一同距x换边，原other属于旧x，不能挂到新位置冒称BiDi重合；
                    // 同x只换样式亲和则保留真实的另一逻辑候选，不丢隐藏语法边缘事实。
                    if primary_caret_x != caret.baseline.x {
                        answer.other_source_affinity = None;
                        answer.other_source_utf16 = None;
                    }
                    answer.source_affinity = affinity;
                    primary_caret_x = caret.baseline.x;
                    primary_inside_part = true;
                } else if delta == distance
                    && primary_caret_x == caret.baseline.x
                    && let Some(answer) = &mut answer
                    && answer.source_utf16 != position
                {
                    // 只有同x的真实不同端点才构成逻辑歧义；异x普通平局沿原稳定主候选结束，
                    // 不按字符宽度插值、不新增方向猜测，也不把合法的字符中点交给外层拒绝。
                    if answer
                        .other_source_utf16
                        .is_some_and(|other| other != position)
                    {
                        return Err(NativeMarkdownComposedViewportError::Pending);
                    } // 多个零advance/GCB重合时不丢中间逻辑候选，等待后继完整affinity生产者。
                    answer.other_source_affinity = Some(affinity);
                    answer.other_source_utf16 = Some(position); // 不丢BiDi边界歧义，亦不声称这是选择anchor/head。
                }
                if position == *range.end()
                    || part.handle.facts().kind == MarkdownComposedLinePartKind::ParagraphBreak
                    || part.handle.facts().kind == MarkdownComposedLinePartKind::FoldedSection
                {
                    break;
                } // 换段part的起点是本行末caret，末端属于下一视觉行；CRLF零advance不能凭空制造BiDi歧义。
                // 其他part仍遍历原GCB端点，保留真实同x候选；不跨字体/方向目录猜位置。
                let next = self.next_boundary(position)?;
                if next <= position || next > *range.end() {
                    return Err(NativeMarkdownComposedViewportError::InvalidRange);
                }
                position = next;
            }
        }
        self.require(current)?;
        answer.ok_or(NativeMarkdownComposedViewportError::Pending)
    }

    /// 按原视觉part流式交付一个逻辑选区的可见框，不建立全部选区/GCB/正文Vec。
    /// visitor可绘入未发布候选；任何Err都必须丢弃整个候选，不能发布成功前缀。
    ///
    /// # Errors
    /// 范围越界、Source/Font/scene失效或visitor失败时整次拒绝；离屏部分明确不产生框。
    pub fn selection_rects(
        &self,
        selection: Range<usize>,
        current: &dyn Fn() -> bool,
        mut visitor: impl FnMut(NSRect) -> Result<()>,
    ) -> Result<usize> {
        self.require(current)?;
        if selection.start > selection.end || selection.end > self.utf16_len() {
            return Err(NativeMarkdownComposedViewportError::InvalidRange);
        }
        let mut count = 0usize;
        // 每个已准备part最多交付一个区间框；外层可对有限多选逐个调用并统一限制frame工作量。
        for line in &self.lines {
            if !self.visible(line.frame) {
                continue;
            }
            for part in &line.parts {
                let facts = part.handle.facts();
                if matches!(
                    facts.kind,
                    MarkdownComposedLinePartKind::FoldedSection
                        | MarkdownComposedLinePartKind::TableRow
                ) {
                    continue; // 隐区没有可见字形或选区框，不能沿其原 UTF16 长度遍历内部 GCB。
                }
                let start = selection.start.max(facts.source_utf16.start);
                let end = selection.end.min(facts.source_utf16.end);
                if start >= end {
                    continue;
                } // 该视觉part没有选中字节，不能通过取整制造额外高亮。
                // 先保原完整字素，再裁回真实绘制项；整形 seam 只影响包络，不拆用户的选择。
                let actual = self.boundary(start, false)?.max(facts.source_utf16.start)
                    ..self.boundary(end, true)?.min(facts.source_utf16.end);
                let rect = self.part_rect(line, part, actual, current)?;
                let Some(rect) = clip_selection(rect, self.clip()) else {
                    continue;
                };
                self.require(current)?;
                visitor(rect)?; // Reader租约在上面的几何函数结束，不跨外层Native绘制/回调。
                self.require(current)?;
                count += 1;
            }
        }
        Ok(count)
    }

    fn part_for_position(&self, offset: usize) -> Result<(&Line, &Part)> {
        for line in &self.lines {
            for part in &line.parts {
                let range = &part.handle.facts().source_utf16;
                if range.contains(&offset)
                    && !matches!(
                        part.handle.facts().kind,
                        MarkdownComposedLinePartKind::FoldedSection
                            | MarkdownComposedLinePartKind::TableRow
                    )
                {
                    return Ok((line, part));
                }
            }
        }
        Err(NativeMarkdownComposedViewportError::Pending) // 本分支只定位非空首scalar；caret由真实换行侧单独查询。
    }

    pub(super) fn boundary(&self, offset: usize, following: bool) -> Result<usize> {
        let mut reader = self
            .output
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownComposedViewportError::Busy)?;
        if reader.grapheme_ordinal(offset)?.is_some() {
            return Ok(offset);
        } // 原边界保持原坐标，避免把已对齐的范围扩大到相邻字素。
        let query = if following {
            MarkdownGraphemeQuery::Next
        } else {
            MarkdownGraphemeQuery::Previous
        };
        match reader.grapheme_query(offset, query)? {
            MarkdownGraphemeAnswer::Known(Some(boundary)) => Ok(boundary.utf16),
            _ => Err(NativeMarkdownComposedViewportError::InvalidRange), // 未知或不存在的GCB不能按相邻UTF16猜。
        }
    }
    fn next_boundary(&self, offset: usize) -> Result<usize> {
        let mut reader = self
            .output
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownComposedViewportError::Busy)?;
        match reader.grapheme_query(offset, MarkdownGraphemeQuery::Next)? {
            MarkdownGraphemeAnswer::Known(Some(boundary)) => Ok(boundary.utf16),
            _ => Err(NativeMarkdownComposedViewportError::InvalidRange), // 真EOF与缺失目录不制造下一字素。
        }
    }

    /// 只借原目录确定这个真实 part 内第一/最后的 GCB；不造新边界或缓存逐字素数组。
    fn part_grapheme_range(&self, part: &Part) -> Result<Option<std::ops::RangeInclusive<usize>>> {
        let range = &part.handle.facts().source_utf16;
        let first = self.boundary(range.start, true)?;
        let last = self.boundary(range.end, false)?;
        Ok((first <= last).then_some(first..=last))
    }
    pub(super) fn part_rect(
        &self,
        line: &Line,
        part: &Part,
        selection: Range<usize>,
        current: &dyn Fn() -> bool,
    ) -> Result<NSRect> {
        let actual = &part.handle.facts().source_utf16;
        if selection.start < actual.start || selection.end > actual.end {
            return Err(NativeMarkdownComposedViewportError::InvalidRange);
        }
        let guarded = || self.require(current).is_ok(); // Native GDEF查询只拿bool，外层仍优先重报实际Source错误。
        let position = |at| {
            let edge = if at == actual.start {
                Some(ComposedPartSourceEdge::Start)
            } else if at == actual.end {
                Some(ComposedPartSourceEdge::End)
            } else {
                None
            };
            if let Some(edge) = edge {
                composed_part_extent_edge(
                    &self.output,
                    &part.handle,
                    &self.palette[part.font],
                    edge,
                    line.baseline,
                    &guarded,
                )
            } else {
                // 真正位于项内部的位置仍是公开 caret 合同，只允许原 GCB/GDEF，不能按 scalar 插值。
                composed_part_caret(
                    &self.output,
                    &part.handle,
                    &self.palette[part.font],
                    at,
                    line.baseline,
                    &guarded,
                )
                .map(|caret| caret.baseline)
            }
        };
        let start = position(selection.start);
        self.require(current)?;
        let start = start?;
        let end = position(selection.end);
        self.require(current)?;
        let end = end?;
        // RTL端点按实际视觉x取min/max；高度沿同一行真实metrics，不能用UTF16长度推宽度。
        let rect = NSRect::new(
            NSPoint::new(start.x.min(end.x), line.baseline.y - line.descent),
            NSSize::new((end.x - start.x).abs(), line.ascent + line.descent),
        );
        if !point(rect.origin)
            || !rect.size.width.is_finite()
            || !rect.size.height.is_finite()
            || rect.size.height <= 0.0
        {
            return Err(NativeMarkdownComposedViewportError::InvalidGeometry);
        }
        Ok(rect)
    }
}

/// 点击和悬停共享同一实际LINK标签包络；显露语法与零advance不获得额外激活区域。
/// 负advance只交换左右边，仍保原行的真实字体高，不按UTF16个数或字形平均宽度猜测。
fn link_part_rect(line: &Line, part: &Part) -> Option<NSRect> {
    let facts = part.handle.facts();
    let styles = facts.presentation;
    if !styles.contains(MarkdownPresentationStyles::LINK)
        || styles.contains(MarkdownPresentationStyles::HIDDEN_MARKER)
        || styles.contains(MarkdownPresentationStyles::MARKER)
    {
        return None;
    }
    let start = line.baseline.x + facts.origin_x_26_6 as f64 / 64.0;
    let end = start + facts.x_advance_26_6 as f64 / 64.0;
    Some(NSRect::new(
        NSPoint::new(start.min(end), line.baseline.y - line.descent),
        NSSize::new((end - start).abs(), line.ascent + line.descent),
    ))
}

fn inside(point: NSPoint, clip: NSRect) -> bool {
    point.x >= clip.origin.x
        && point.x < clip.origin.x + clip.size.width
        && point.y >= clip.origin.y
        && point.y < clip.origin.y + clip.size.height
}

pub(super) fn clip_selection(rect: NSRect, clip: NSRect) -> Option<NSRect> {
    let left = rect.origin.x.max(clip.origin.x);
    let bottom = rect.origin.y.max(clip.origin.y);
    let right = (rect.origin.x + rect.size.width).min(clip.origin.x + clip.size.width);
    let top = (rect.origin.y + rect.size.height).min(clip.origin.y + clip.size.height);
    if top <= bottom || right < left || (rect.size.width > 0.0 && right == left) {
        return None;
    }
    Some(NSRect::new(
        NSPoint::new(left, bottom),
        NSSize::new(right - left, top - bottom),
    ))
}
