//! 有限可见行的真实GCB几何；不按UTF16平均分配，不保留整行字素列表。

use super::*;
use crate::native_surface::paged_line_geometry::composed_part_caret;
use omni_markdown_core::{
    MarkdownComposedLinePartKind, MarkdownGraphemeAnswer, MarkdownGraphemeQuery,
};

/// 一次命中最多查询固定数量的真实边界；超大零advance行由后继producer另行索引。
const HIT_BOUNDARIES: usize = 4_096;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NativeMarkdownComposedViewportHit {
    pub source_utf16: usize,
    /// 两个逻辑位置与点击点同距时保留另一候选，外层不能据此伪造主方向。
    pub other_source_utf16: Option<usize>,
    pub line_ordinal: usize,
}

impl NativeMarkdownComposedViewport {
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
            .filter(|end| *end <= self.key.request.input.utf16_len)
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
        let (line, part) = self.part_for_position(range.location)?;
        if !self.visible(line.frame) {
            return Err(NativeMarkdownComposedViewportError::Pending);
        }
        // 非空请求扩到首scalar所属完整GCB；不把后继行或UTF16中点当作字形首位置。
        let start = self.boundary(range.location, false)?;
        let next = self.next_boundary(start)?;
        if next > part.handle.facts().source_utf16.end || next <= start {
            return Err(NativeMarkdownComposedViewportError::InvalidRange);
        }
        let actual = start..next;
        let rect = self.part_rect(line, part, actual.clone(), current)?;
        if !self.visible(rect) && rect.size.width != 0.0 {
            return Err(NativeMarkdownComposedViewportError::Pending);
        }
        self.require(current)?;
        Ok(NativeMarkdownPagedCharacterRect {
            rect,
            actual_range: NSRange::new(actual.start, actual.end - actual.start),
        })
    }

    /// 有界顺序比较原GCB的实际caret，保留BiDi同视觉位置的两个逻辑候选。
    ///
    /// # Errors
    /// 点在窗口外、候选行尚未准备、边界预算不足或原权限失效时返回Pending/错误。
    pub fn hit(
        &self,
        local: NSPoint,
        current: &dyn Fn() -> bool,
    ) -> Result<NativeMarkdownComposedViewportHit> {
        self.require(current)?;
        if !point(local) || !inside(local, self.key.request.viewport) {
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
        // 先以原目录rank预检全行查询量；没有预算时不能用已比较的前缀冒称最近位置。
        let mut count = 0usize;
        {
            let mut reader = self
                .output
                .try_borrow_mut()
                .map_err(|_| NativeMarkdownComposedViewportError::Busy)?;
            for part in &line.parts {
                let start = reader
                    .grapheme_ordinal(part.handle.facts().source_utf16.start)?
                    .ok_or(NativeMarkdownComposedViewportError::InvalidRange)?;
                let end = reader
                    .grapheme_ordinal(part.handle.facts().source_utf16.end)?
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
        // 有限GCB逐项比较真实caret；不假设连字内部GDEF单调，也不按源字节长度插值。
        for part in &line.parts {
            let mut position = part.handle.facts().source_utf16.start;
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
                let delta = (caret.baseline.x - local.x).abs();
                if delta < distance {
                    // 更近的视觉位置整体替换旧候选，旧BiDi歧义不能污染新结果。
                    distance = delta;
                    answer = Some(NativeMarkdownComposedViewportHit {
                        source_utf16: position,
                        other_source_utf16: None,
                        line_ordinal: line.handle.facts().ordinal,
                    });
                } else if delta == distance
                    && let Some(answer) = &mut answer
                    && answer.source_utf16 != position
                {
                    if answer
                        .other_source_utf16
                        .is_some_and(|other| other != position)
                    {
                        return Err(NativeMarkdownComposedViewportError::Pending);
                    } // 多个零advance/GCB重合时不丢中间逻辑候选，等待后继完整affinity生产者。
                    answer.other_source_utf16 = Some(position); // 不丢BiDi边界歧义，亦不声称这是选择anchor/head。
                }
                if position == part.handle.facts().source_utf16.end
                    || part.handle.facts().kind == MarkdownComposedLinePartKind::ParagraphBreak
                {
                    break;
                } // 换段part的起点是本行末caret，末端属于下一视觉行；CRLF零advance不能凭空制造BiDi歧义。
                // 其他part仍遍历原GCB端点，保留真实同x候选；不跨字体/方向目录猜位置。
                let next = self.next_boundary(position)?;
                if next <= position || next > part.handle.facts().source_utf16.end {
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
        if selection.start > selection.end || selection.end > self.key.request.input.utf16_len {
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
                let start = selection.start.max(facts.source_utf16.start);
                let end = selection.end.min(facts.source_utf16.end);
                if start >= end {
                    continue;
                } // 该视觉part没有选中字节，不能通过取整制造额外高亮。
                let actual = self.boundary(start, false)?..self.boundary(end, true)?;
                let rect = self.part_rect(line, part, actual, current)?;
                let Some(rect) = clip_selection(rect, self.key.request.viewport) else {
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
                if range.contains(&offset) {
                    return Ok((line, part));
                }
            }
        }
        Err(NativeMarkdownComposedViewportError::Pending) // 本分支只定位非空首scalar；caret由真实换行侧单独查询。
    }

    fn boundary(&self, offset: usize, following: bool) -> Result<usize> {
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
    fn part_rect(
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
        let start = composed_part_caret(
            &self.output,
            &part.handle,
            &self.palette[part.font],
            selection.start,
            line.baseline,
            &guarded,
        );
        self.require(current)?;
        let start = start?;
        let end = composed_part_caret(
            &self.output,
            &part.handle,
            &self.palette[part.font],
            selection.end,
            line.baseline,
            &guarded,
        );
        self.require(current)?;
        let end = end?;
        // RTL端点按实际视觉x取min/max；高度沿同一行真实metrics，不能用UTF16长度推宽度。
        let rect = NSRect::new(
            NSPoint::new(
                start.baseline.x.min(end.baseline.x),
                line.baseline.y - line.descent,
            ),
            NSSize::new(
                (end.baseline.x - start.baseline.x).abs(),
                line.ascent + line.descent,
            ),
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

fn inside(point: NSPoint, clip: NSRect) -> bool {
    point.x >= clip.origin.x
        && point.x < clip.origin.x + clip.size.width
        && point.y >= clip.origin.y
        && point.y < clip.origin.y + clip.size.height
}

fn clip_selection(rect: NSRect, clip: NSRect) -> Option<NSRect> {
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
