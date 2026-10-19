//! 从封存行取真实插入点两侧；换行侧不能代替双向文字的第二视觉位置。

use super::*;
use crate::native_surface::{
    paged_input::NativeMarkdownBidiCaretSide, paged_line_geometry::composed_part_caret,
    paged_text_view::NativeMarkdownPagedHeadAffinity,
};

/// 只有有限标量，不保留Native、正文或新索引；两个框不同时，消费者必须明确解决歧义。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NativeMarkdownComposedCaretGeometry {
    pub source_utf16: usize,
    pub line_ordinal: usize,
    pub line_frame: NSRect,
    pub affinity: NativeMarkdownPagedHeadAffinity,
    pub primary: NSRect,
    pub secondary: Option<NSRect>,
}

impl NativeMarkdownComposedViewport {
    /// 读取一个真实GCB的行框和视觉插入点；只访问已经准备的有限Line/Part。
    ///
    /// Upstream取终止于该位置的原行，Downstream取从该位置开始的原行。真正BOF/EOF
    /// 只有一个存在的文档侧；缓存窗口边缘没有这个特权。行内BiDi边界仍可有两个框，
    /// 不因调用者给了换行侧就静默丢掉第二位置。准备好的overscan行也可返回，屏幕与
    /// IME消费者还须按自身原viewport合同检查可见性。
    ///
    /// # Errors
    /// 原Source/Font/scene失效、非GCB、请求侧未准备或多于两个不同位置均拒绝。
    pub fn caret_geometry(
        &self,
        source_utf16: usize,
        affinity: NativeMarkdownPagedHeadAffinity,
        current: &dyn Fn() -> bool,
    ) -> Result<NativeMarkdownComposedCaretGeometry> {
        self.caret_geometry_impl(source_utf16, affinity, None, current)
    }

    /// 正式选择的Bidi侧决定实际主caret；没有旧侧时与视觉导航共用最低真实level规则。
    /// 已失配的侧保持Pending，不能在同一次旧输入意图中改签另一个part。
    pub fn caret_geometry_with_side(
        &self,
        source_utf16: usize,
        affinity: NativeMarkdownPagedHeadAffinity,
        side: Option<NativeMarkdownBidiCaretSide>,
        current: &dyn Fn() -> bool,
    ) -> Result<NativeMarkdownComposedCaretGeometry> {
        self.caret_geometry_impl(source_utf16, affinity, Some(side), current)
    }

    fn caret_geometry_impl(
        &self,
        source_utf16: usize,
        affinity: NativeMarkdownPagedHeadAffinity,
        side: Option<Option<NativeMarkdownBidiCaretSide>>,
        current: &dyn Fn() -> bool,
    ) -> Result<NativeMarkdownComposedCaretGeometry> {
        self.require(current)?;
        let index = self.caret_line_index(source_utf16, affinity)?;
        self.require(current)?; // 原Reader租约已结束，真实Source错误仍先于缺页或撤权。
        let line = &self.lines[index];
        let selected = side
            .map(|side| self.caret_part_index(line, source_utf16, side))
            .transpose()?;
        let (mut primary, mut secondary) = (None, None);
        // Part本身已有固定窗口上限；只查包含该GCB的实际字体/方向端点，不遍历正文。
        for (index, part) in line.parts.iter().enumerate() {
            if selected.is_some_and(|selected| selected != index) {
                continue; // 正式caret/IME消费同一次实际Bidi侧；旧公开查询仍保留双位置事实。
            }
            let range = &part.handle.facts().source_utf16;
            if source_utf16 < range.start || source_utf16 > range.end {
                continue;
            } // 同行但没有这个源位置的视觉part不参与插入点选择。
            let caret = composed_part_caret(
                &self.output,
                &part.handle,
                &self.palette[part.font],
                source_utf16,
                line.baseline,
                &|| self.require(current).is_ok(),
            );
            self.require(current)?; // bool回调不抹掉原Source的实际首错。
            let caret = caret?;
            let rect = NSRect::new(
                NSPoint::new(caret.baseline.x, caret.baseline.y - line.descent),
                NSSize::new(0.0, line.ascent + line.descent),
            );
            if !point(rect.origin) || !rect.size.height.is_finite() || rect.size.height <= 0.0 {
                return Err(NativeMarkdownComposedViewportError::InvalidGeometry);
            } // 不能把无效坐标或空字体行补成一个固定高度的成功光标。
            if primary.is_none() {
                primary = Some(rect);
            } else if primary != Some(rect) && secondary.is_none() {
                secondary = Some(rect);
            } else if primary != Some(rect) && secondary != Some(rect) {
                return Err(NativeMarkdownComposedViewportError::Pending);
            } // 保留真实BiDi歧义；同点字体边界只归并相同框，多候选不能任取前两项。
        }
        self.require(current)?;
        Ok(NativeMarkdownComposedCaretGeometry {
            source_utf16,
            line_ordinal: line.handle.facts().ordinal,
            line_frame: line.frame,
            affinity,
            primary: primary.ok_or(NativeMarkdownComposedViewportError::InvalidRange)?,
            secondary,
        })
    }
}
