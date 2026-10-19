//! 现成L1/L2视觉part之间的一GCB导航；不建立第二UBA顺序或正文/字素数组。

use super::*;
use crate::native_surface::{
    paged_input::NativeMarkdownBidiCaretSide,
    paged_text_view::NativeMarkdownPagedHeadAffinity as Affinity,
};
use omni_markdown_core::{MarkdownComposedCaretAssociation, MarkdownComposedLinePartKind};

pub struct NativeMarkdownComposedVisualTarget {
    pub utf16: usize,
    pub affinity: Affinity,
    pub side: NativeMarkdownBidiCaretSide,
}

impl NativeMarkdownComposedViewport {
    /// 原头位置所属行的真实段base；与caret一样按soft-wrap侧和真实EOF选择原Line。
    pub fn paragraph_base_level_at(
        &self,
        utf16: usize,
        affinity: Affinity,
        current: &dyn Fn() -> bool,
    ) -> Result<u8> {
        self.require(current)?;
        let index = self.caret_line_index(utf16, affinity)?;
        let base = self
            .output
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownComposedViewportError::Busy)?
            .paragraph_base_level(&self.lines[index].handle)?;
        self.require(current)?;
        Ok(base)
    }

    /// 真实EOF沿原last ordinal定位；其他位置严格消费soft-wrap侧而不夹到窗口边缘。
    pub(super) fn caret_line_index(&self, utf16: usize, affinity: Affinity) -> Result<usize> {
        let length = self.key.request.input.utf16_len;
        if utf16 > length {
            return Err(NativeMarkdownComposedViewportError::InvalidRange);
        }
        if utf16 == length {
            let last = self
                .output
                .try_borrow_mut()
                .map_err(|_| NativeMarkdownComposedViewportError::Busy)?
                .last_line()?;
            let last = last.ok_or(NativeMarkdownComposedViewportError::Pending)?;
            if last.facts().source_utf16.end != length {
                return Err(NativeMarkdownComposedViewportError::Pending);
            }
            let index = self
                .lines
                .iter()
                .position(|line| line.handle.facts().ordinal == last.facts().ordinal)
                .ok_or(NativeMarkdownComposedViewportError::Pending)?;
            let line = &self.lines[index];
            if !line.handle.facts().source_utf16.is_empty()
                && line.parts.last().is_some_and(|part| {
                    part.handle.facts().kind == MarkdownComposedLinePartKind::ParagraphBreak
                })
            {
                return Err(NativeMarkdownComposedViewportError::Pending);
            } // 真换行后尚无尾空行，不能用前一行替它签caret或导航位置。
            return Ok(index);
        }
        self.lines
            .iter()
            .position(|line| {
                let range = &line.handle.facts().source_utf16;
                if utf16 == 0 {
                    range.start == 0
                } else if affinity.is_upstream() {
                    range.start < utf16 && utf16 <= range.end
                } else {
                    range.start <= utf16 && utf16 < range.end
                }
            })
            .ok_or(NativeMarkdownComposedViewportError::Pending)
    }

    /// 显式侧必须覆盖原位置的相应逻辑字素；缺侧时使用实际最低level的规范主侧。
    pub(super) fn caret_part_index(
        &self,
        line: &Line,
        utf16: usize,
        side: Option<NativeMarkdownBidiCaretSide>,
    ) -> Result<usize> {
        let mut selected: Option<usize> = None;
        // part数已由原窗口预算限制；这里只读已发布元数据，不再读取File或正文。
        for (index, part) in line.parts.iter().enumerate() {
            let facts = part.handle.facts();
            let range = &facts.source_utf16;
            if utf16 < range.start || utf16 > range.end {
                continue;
            }
            if let Some(side) = side {
                let covers = if range.is_empty() {
                    true
                } else if side.logical_after {
                    utf16 < range.end
                } else {
                    range.start < utf16
                };
                if facts.level != side.level || !covers {
                    continue;
                }
            }
            if selected.is_none_or(|old| line.parts[old].handle.facts().level > facts.level) {
                selected = Some(index);
            }
        }
        selected.ok_or(NativeMarkdownComposedViewportError::Pending)
    }

    /// Share one 256-record budget across Line/Part/GCB reads; the original Body resumes missing or unfinished work.
    pub fn visual_neighbor(
        &self,
        utf16: usize,
        affinity: Affinity,
        side: Option<NativeMarkdownBidiCaretSide>,
        right: bool,
        current: &dyn Fn() -> bool,
    ) -> Result<NativeMarkdownComposedVisualTarget> {
        self.require(current)?;
        let _ = self.caret_line_index(utf16, affinity)?; // The starting position must still belong to this actual window.
        let answer = {
            let mut output = self
                .output
                .try_borrow_mut()
                .map_err(|_| NativeMarkdownComposedViewportError::Busy)?;
            let side = side.map(|side| omni_markdown_core::MarkdownComposedCaretSide {
                level: side.level,
                association: if side.logical_after {
                    MarkdownComposedCaretAssociation::After
                } else {
                    MarkdownComposedCaretAssociation::Before
                },
            });
            let affinity = if affinity.is_upstream() {
                omni_markdown_core::MarkdownComposedLineAffinity::Upstream
            } else {
                omni_markdown_core::MarkdownComposedLineAffinity::Downstream
            };
            let mut query = output.visual_query(utf16, affinity, side, right)?;
            match output.step_visual(&mut query, 256) {
                Ok(step) => step.answer,
                Err(omni_markdown_core::MarkdownLineComposeError::InvalidRequest) => {
                    return Err(NativeMarkdownComposedViewportError::Pending);
                }
                Err(cause) => return Err(cause.into()),
            }
        }; // End the sole reader borrow before any Native/current callback.
        self.require(current)?;
        match answer {
            omni_markdown_core::MarkdownComposedVisualAnswer::Found {
                utf16,
                side,
                line,
                ordinal,
            } => {
                if !self
                    .lines
                    .iter()
                    .any(|line| line.handle.facts().ordinal == ordinal)
                {
                    return Err(NativeMarkdownComposedViewportError::Pending);
                }
                Ok(NativeMarkdownComposedVisualTarget {
                    utf16,
                    affinity: if !line.is_empty() && utf16 == line.end && utf16 != 0 {
                        Affinity::Upstream
                    } else {
                        Affinity::Downstream
                    },
                    side: NativeMarkdownBidiCaretSide {
                        level: side.level,
                        logical_after: side.association == MarkdownComposedCaretAssociation::After,
                    },
                })
            }
            _ => Err(NativeMarkdownComposedViewportError::Pending), // Never synchronously loop for more IO; the Body retains and resumes its query.
        }
    }
}
