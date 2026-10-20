//! 非字形控制的真实样式响应；换段不改源字节，Tab不以固定空格数代替制表停靠。
use super::{MarkdownLineComposeError as Error, MarkdownLineRegisteredFont, item::Result};
use crate::MarkdownLayoutTextAuthority;
mod validated;
use std::{ops::Range, sync::Arc};
pub(super) use validated::ValidatedControl;

/// 平台从原段落样式捕获的对齐；非Left需要实际后继宽度/terminator后才可完成。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownLineTabAlignment {
    Left,
    Right,
    Center,
    Decimal,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownLineTabStop {
    pub location_26_6: i64,
    pub alignment: MarkdownLineTabAlignment,
}

/// 候选样式的有界元数据；None明确表示尚未取得系统默认间隔，不隐式猜28点或8空格。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownLineParagraphStyle {
    pub tab_stops: Vec<MarkdownLineTabStop>,
    pub default_tab_interval_26_6: Option<i64>,
}
impl MarkdownLineParagraphStyle {
    pub(super) fn require(&self) -> Result<()> {
        if self.tab_stops.len() > 64
            || self
                .default_tab_interval_26_6
                .is_some_and(|value| value < 0)
            || self.tab_stops.iter().any(|stop| stop.location_26_6 < 0)
            || self
                .tab_stops
                .windows(2)
                .any(|pair| pair[0].location_26_6 >= pair[1].location_26_6)
        {
            return Err(Error::InvalidRequest);
        } // 超出候选元数据预算明确失败，不截断真实stop数组，也不冒称功能完备。
        Ok(())
    }
    pub(super) fn tab(&self, pen: i64) -> Result<MarkdownLineTabPlacement> {
        self.require()?;
        if let Some(stop) = self.tab_stops.iter().find(|stop| stop.location_26_6 > pen) {
            if stop.alignment != MarkdownLineTabAlignment::Left {
                return Ok(MarkdownLineTabPlacement::NeedsLookahead {
                    pen_26_6: pen,
                    stop: *stop,
                });
            }
            return Ok(MarkdownLineTabPlacement::Left {
                advance_26_6: stop.location_26_6.checked_sub(pen).ok_or(Error::Overflow)?,
            });
        }
        let Some(interval) = self.default_tab_interval_26_6 else {
            return Ok(MarkdownLineTabPlacement::NeedsDefaultInterval);
        };
        if interval == 0 {
            return Ok(MarkdownLineTabPlacement::Left { advance_26_6: 0 });
        }
        // 真实Cocoa对照：隐式网格以最后一个显式stop为原点，不是以全文x=0的倍数。
        let anchor = self.tab_stops.last().map_or(0, |stop| stop.location_26_6);
        let distance = pen.checked_sub(anchor).ok_or(Error::Overflow)?;
        let target = distance
            .div_euclid(interval)
            .checked_add(1)
            .and_then(|value| value.checked_mul(interval))
            .and_then(|value| value.checked_add(anchor))
            .ok_or(Error::Overflow)?;
        Ok(MarkdownLineTabPlacement::Left {
            advance_26_6: target.checked_sub(pen).ok_or(Error::Overflow)?,
        })
    }
}

/// 未得right/center/decimal真实后继前仍是待决；该值不代表源范围或绘制权限。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownLineTabPlacement {
    Left {
        advance_26_6: i64,
    },
    NeedsLookahead {
        pen_26_6: i64,
        stop: MarkdownLineTabStop,
    },
    NeedsDefaultInterval,
}

/// 非字形part仍保留实际Source/Font/GCB；Native跳过画glyph不能跳过几何和选择。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownComposedLinePartKind {
    Glyphs,
    Tab,
    ParagraphBreak,
    /// 原折叠控制覆盖的 canonical 隐区；没有内部可见 glyph 或可命中的 caret。
    FoldedSection,
    /// 已按最终列宽完成真实格排版的表格块行，不包含正文 glyph。
    TableRow,
}
impl MarkdownComposedLinePartKind {
    pub(super) fn code(self) -> u8 {
        match self {
            Self::Glyphs => 0,
            Self::Tab => 1,
            Self::ParagraphBreak => 2,
            Self::FoldedSection => 3,
            Self::TableRow => 4,
        }
    }
    pub(super) fn decode(code: u8) -> Result<Self> {
        match code {
            0 => Ok(Self::Glyphs),
            1 => Ok(Self::Tab),
            2 => Ok(Self::ParagraphBreak),
            3 => Ok(Self::FoldedSection),
            4 => Ok(Self::TableRow),
            _ => Err(Error::InconsistentCoverage),
        }
    }
}

/// 响应只持一个实际注册Font和有限段落样式；不捕获Native对象、文档、历史或第二正文。
pub struct MarkdownLineControlResponse {
    pub(super) request: super::MarkdownLineInlineControl,
    pub(super) registered: MarkdownLineRegisteredFont,
    pub(super) points: f32,
    pub(super) paragraph: MarkdownLineParagraphStyle,
    pub(super) presentation: crate::MarkdownPresentationStyles,
    pub(super) paragraph_inset_26_6: i32,
}
impl MarkdownLineControlResponse {
    /// 原控制范围也消费同一来源语义；换行不画墨迹，但其字体和段落不得把隐藏围栏重新撑高。
    pub fn with_presentation(
        mut self,
        styles: crate::MarkdownPresentationStyles,
        inset_26_6: i32,
    ) -> Result<Self> {
        if inset_26_6 < 0 {
            return Err(Error::InvalidRequest);
        }
        self.presentation = styles;
        self.paragraph_inset_26_6 = inset_26_6;
        Ok(self)
    }
}
impl super::MarkdownLineInlineControl {
    /// 原控制nonce和源票随响应一起归还；公开事实的改写不能替代实际仍待处理的请求。
    pub fn respond(
        self,
        registered: &MarkdownLineRegisteredFont,
        points: f32,
        paragraph: MarkdownLineParagraphStyle,
    ) -> Result<MarkdownLineControlResponse> {
        paragraph.require()?;
        if !points.is_finite() || points <= 0.0 {
            return Err(Error::InvalidRequest);
        }
        registered.registration().lease()?;
        Ok(MarkdownLineControlResponse {
            request: self,
            registered: registered.clone(),
            points,
            paragraph,
            presentation: crate::MarkdownPresentationStyles::default(),
            paragraph_inset_26_6: 0,
        })
    }
    pub fn source(&self) -> &MarkdownLayoutTextAuthority {
        &self.source
    }
    pub(super) fn new(
        source: MarkdownLayoutTextAuthority,
        range: Range<usize>,
        kind: super::MarkdownLineInlineControlKind,
    ) -> Self {
        Self {
            source_utf16: range,
            kind,
            source,
            nonce: Arc::new(()),
        }
    }
    pub(super) fn accepts(&self, response: &MarkdownLineControlResponse) -> bool {
        Arc::ptr_eq(&self.nonce, &response.request.nonce)
            && self.source == response.request.source
            && self.source_utf16 == response.request.source_utf16
            && self.kind == response.request.kind
    }
}

#[cfg(test)]
mod tests;
