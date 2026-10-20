//! 同一真实LineEnd的对齐占位；尾空白只退出对齐宽度，不退出字形、光标或选择域。

use super::*;
use omni_markdown_core::{
    MarkdownComposedLineTrailingWhitespace, MarkdownComposedLineTrailingWhitespaceStep,
    MarkdownLineComposeBudget, MarkdownShapingDirection, MarkdownTableAlignment,
};

/// 只携原弱行句柄及已完成的真实可见推进范围；调用方不能用任意float构造对齐结果。
pub struct NativeMarkdownMeasuredLine {
    handle: MarkdownComposedLine,
    extent: Option<Extent>,
}

pub(super) struct Extent {
    left: f64,
    right: f64,
}

impl From<MarkdownComposedLine> for NativeMarkdownMeasuredLine {
    fn from(handle: MarkdownComposedLine) -> Self {
        Self {
            handle,
            extent: None,
        } // 原literal/Left入口无需测尾；Center/Right在消费时必须持真实完成的extent。
    }
}

impl NativeMarkdownMeasuredLine {
    pub fn facts(&self) -> &omni_markdown_core::MarkdownComposedLineFacts {
        self.handle.facts()
    }

    pub(in crate::native_surface) fn handle(&self) -> &MarkdownComposedLine {
        &self.handle
    }

    pub(super) fn into_parts(self) -> (MarkdownComposedLine, Option<Extent>) {
        (self.handle, self.extent)
    }
}

pub(super) fn alignment_origin_x(
    alignment: MarkdownTableAlignment,
    width: f64,
    inset: f64,
    extent: Option<&Extent>,
) -> Result<f64> {
    // 可用宽仍是本次真实排版 lane，不能用横向 clip 或整个 ScrollDocument 宽代替。
    // 对齐只平移原封存行：全部尾空白和原选择域仍随同一 baseline 移动。
    let available = width - inset * 2.0;
    if !width.is_finite() || !inset.is_finite() || available <= 0.0 || width >= MAX_POINT {
        return Err(NativeMarkdownComposedViewportError::InvalidGeometry);
    }
    if alignment == MarkdownTableAlignment::Left {
        return Ok(inset); // 左对齐不消费尾空白测量，也不改变原软行推进或选择端点。
    }
    let extent = extent.ok_or(NativeMarkdownComposedViewportError::Pending)?;
    // RTL 的逻辑尾空白可能在视觉左侧。除了排可见宽，还必须减真实 left，
    // 否则绘图看似居中，但该格 caret、Find 和点击会多出尾空白的整段偏移。
    let remaining = available - (extent.right - extent.left);
    let offset = match alignment {
        MarkdownTableAlignment::Left => unreachable!("左对齐已经沿原入口返回"),
        MarkdownTableAlignment::Center => remaining / 2.0,
        MarkdownTableAlignment::Right => remaining,
    };
    let origin = inset + offset - extent.left;
    if !origin.is_finite() || origin.abs() >= MAX_POINT {
        return Err(NativeMarkdownComposedViewportError::InvalidGeometry);
    }
    Ok(origin)
}

/// 只归原LineMetrics持有的尾查询；不保Reader、Font、Source强边或另一份行目录。
pub(in crate::native_surface) struct NativeMarkdownLineAlignmentMeasure {
    query: MarkdownComposedLineTrailingWhitespace,
    range: Range<usize>,
    ordinal: usize,
    parts: usize,
    observed: usize,
    trim_end: Option<usize>,
    extent: Option<Extent>,
}

impl NativeMarkdownLineAlignmentMeasure {
    pub(in crate::native_surface) fn new(
        output: &RefCell<MarkdownComposedLines>,
        line: &MarkdownComposedLine,
    ) -> Result<Self> {
        // begin 只封存原弱身份；真正 Source/目录读取全部留给后继有预算的 step。
        // 这份度量不跨尺寸换代复用，完成后只随当前有限可见 handle 一起发布。
        let query = output
            .try_borrow()
            .map_err(|_| NativeMarkdownComposedViewportError::Busy)?
            .begin_line_trailing_whitespace(line)?;
        Ok(Self {
            query,
            range: line.facts().source_utf16.clone(),
            ordinal: line.facts().ordinal,
            parts: line.facts().parts,
            observed: 0,
            trim_end: None,
            extent: None,
        })
    }

    /// 原Source/GCB尾查询使用调用者剩余量子；完成后只保真实UTF16末端，不能从字形id猜空格。
    pub(in crate::native_surface) fn step_source(
        &mut self,
        output: &RefCell<MarkdownComposedLines>,
        line: &MarkdownComposedLine,
        budget: MarkdownLineComposeBudget,
    ) -> Result<MarkdownComposedLineTrailingWhitespaceStep> {
        let step = output
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownComposedViewportError::Busy)?
            .step_line_trailing_whitespace(line, &mut self.query, budget)?;
        self.trim_end = step.answer;
        Ok(step)
    }

    pub(in crate::native_surface) fn source_ready(&self) -> bool {
        self.trim_end.is_some()
    }

    /// 离屏Find消费同一完成extent；RTL尾空白在视觉左侧时也使用真实left，而非只减宽度。
    pub(in crate::native_surface) fn origin_x(
        &self,
        alignment: MarkdownTableAlignment,
        width: f64,
        inset: f64,
    ) -> Result<f64> {
        // Find 也等待同一整行量度，不能只遇到目标 part 就用它的局部 advance 居中。
        // 已完成且全空白的行确实没有可见占位，不把它误判成缺失字体或未知宽度。
        if self.observed != self.parts || self.trim_end.is_none() {
            return Err(NativeMarkdownComposedViewportError::Pending);
        }
        if self.trim_end == Some(self.range.start) {
            return alignment_origin_x(
                alignment,
                width,
                inset,
                Some(&Extent {
                    left: 0.0,
                    right: 0.0,
                }),
            );
        }
        alignment_origin_x(alignment, width, inset, self.extent.as_ref())
    }

    /// 复用原字体与原sealed part的caret，不重排或trim正文；逐part沿原metadata预算推进。
    pub(in crate::native_surface) fn observe_part(
        &mut self,
        output: &RefCell<MarkdownComposedLines>,
        part: &MarkdownComposedLinePart,
        font: &NativeMarkdownLineFont,
        current: &dyn Fn() -> bool,
    ) -> Result<()> {
        let trim_end = self
            .trim_end
            .ok_or(NativeMarkdownComposedViewportError::Pending)?;
        let facts = part.facts();
        if self.observed == self.parts
            || facts.source_utf16.start < self.range.start
            || facts.source_utf16.end > self.range.end
        {
            return Err(NativeMarkdownComposedViewportError::InvalidCoverage);
        }
        if facts.source_utf16.start < trim_end {
            // 完整 part 的视觉推进由原 sealed facts 给出；这里不读整行文本、字形数组，
            // 也不按 glyph id、名义 space 宽或 UTF16 长度推测实际空白。
            let mut left = facts.origin_x_26_6 as f64 / 64.0;
            let mut right = left + facts.x_advance_26_6 as f64 / 64.0;
            if trim_end < facts.source_utf16.end {
                // Core 已将末端提升到原完整 GCB。混合 Prepend/空白整簇全部保留，
                // 只有纯尾空白退出占位；现 caret 的 GDEF/cluster 规则仍是唯一几何来源。
                let caret = super::super::paged_line_geometry::composed_part_caret(
                    output,
                    part,
                    font,
                    trim_end,
                    CGPoint::ZERO,
                    current,
                );
                // 字体调用后先核真实Source/Reader首错，不能把同次EIO改写为前台撤权。
                output
                    .try_borrow()
                    .map_err(|_| NativeMarkdownComposedViewportError::Busy)?
                    .require(part.source_authority())?;
                let caret = caret?;
                match facts.direction {
                    MarkdownShapingDirection::LeftToRight => right = caret.baseline.x,
                    MarkdownShapingDirection::RightToLeft => left = caret.baseline.x,
                    _ => return Err(NativeMarkdownComposedViewportError::InvalidGeometry),
                }
            }
            if !left.is_finite() || !right.is_finite() || right < left {
                return Err(NativeMarkdownComposedViewportError::InvalidGeometry);
            }
            if let Some(extent) = self.extent.as_mut() {
                // 原视觉 part 可有混合方向；联合实际左右边缘，不能把多个逻辑 advance
                // 相加后假定从零开始，也不能用最后一个逻辑 part 代替整行占位。
                extent.left = extent.left.min(left);
                extent.right = extent.right.max(right);
            } else {
                self.extent = Some(Extent { left, right });
            }
        }
        // 尾部 part 仍被真实遍历和计量，但不参加对齐占位；它的完整 handle/字形保持，
        // 后继点击、选择和输入法仍能到达这些空格，不在此创建裁剪过的第二条行。
        self.observed += 1;
        Ok(())
    }

    /// 完整原行才签具体MeasuredLine；全空白行使用真实空占位，原尾空白glyph仍保留在handle中。
    pub(in crate::native_surface) fn finish(
        self,
        handle: MarkdownComposedLine,
    ) -> Result<NativeMarkdownMeasuredLine> {
        // 句柄与完成度在原唯一 producer 内配对，外部不能拿别行或任意浮点值签对齐票。
        // 关闭输出会自然撤销同一弱句柄；MeasuredLine 不保 Source、FD、字体或 Reader。
        if self.trim_end.is_none()
            || self.observed != self.parts
            || handle.facts().source_utf16 != self.range
            || handle.facts().ordinal != self.ordinal
            || handle.facts().parts != self.parts
        {
            return Err(NativeMarkdownComposedViewportError::Pending);
        }
        let extent = match self.extent {
            Some(extent) => extent,
            None if self.trim_end == Some(self.range.start) => Extent {
                left: 0.0,
                right: 0.0,
            },
            None => return Err(NativeMarkdownComposedViewportError::InvalidCoverage),
        };
        Ok(NativeMarkdownMeasuredLine {
            handle,
            extent: Some(extent),
        })
    }
}
