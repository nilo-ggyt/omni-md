//! 已核原控制nonce、真实GCB与段落范围的唯一响应；只在原worker内部流转。
use super::*;
use crate::MarkdownLayoutTextSource;
use crate::markdown_line_composer::{MarkdownLineFontCatalog, MarkdownLineItemFacts};

pub(in crate::markdown_line_composer) struct ValidatedControl {
    pub(in crate::markdown_line_composer) request: super::super::MarkdownLineInlineControl,
    pub(in crate::markdown_line_composer) registered: MarkdownLineRegisteredFont,
    pub(in crate::markdown_line_composer) facts: MarkdownLineItemFacts,
    pub(in crate::markdown_line_composer) paragraph: MarkdownLineParagraphStyle,
    pub(in crate::markdown_line_composer) presentation: crate::MarkdownPresentationStyles,
    pub(in crate::markdown_line_composer) paragraph_inset_26_6: i32,
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    pub(in crate::markdown_line_composer) table:
        Option<super::super::engine::table_row::TableGeometry>,
}
impl ValidatedControl {
    pub(in crate::markdown_line_composer) fn require(
        &self,
        source: &MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        fonts: &MarkdownLineFontCatalog,
    ) -> Result<()> {
        source.health()?;
        if !source.permits(authority)
            || self.request.source() != authority
            || self.request.source_utf16 != self.facts.source_utf16
            || self.facts.source_utf16.end > source.utf16_len()
            || self.facts.source_utf16.start < self.facts.paragraph_utf16.start
            || self.facts.source_utf16.end > self.facts.paragraph_utf16.end
            || !self.facts.points.is_finite()
            || (self.facts.points <= 0.0
                && self.request.kind != super::super::MarkdownLineInlineControlKind::TableRow)
        {
            return Err(Error::SourceMismatch);
        }
        let actual = fonts.require_registration(&self.registered.registration())?;
        if !actual.font().permits(&self.registered.font().authority()) {
            return Err(Error::SourceMismatch);
        }
        self.paragraph.require()?;
        source.health().map_err(Error::from) // 窗口命中、EOF和字体注册命中都不能跳过原源永久I/O失败。
    }
    pub(in crate::markdown_line_composer) fn from_response(
        response: MarkdownLineControlResponse,
        facts: MarkdownLineItemFacts,
    ) -> Self {
        Self {
            request: response.request,
            registered: response.registered,
            facts,
            paragraph: response.paragraph,
            presentation: response.presentation,
            paragraph_inset_26_6: response.paragraph_inset_26_6,
            #[cfg(all(
                feature = "s2-table-window-prototype",
                feature = "paged-shaping-frontier"
            ))]
            table: None,
        }
    }
}

impl ValidatedControl {
    /// 真空文档或真实末尾分隔符之后的空段；调用者仍须持同一worker生成的私有EOF nonce。
    pub(in crate::markdown_line_composer) fn empty(
        request: &super::super::MarkdownLineInlineControl,
        response: MarkdownLineControlResponse,
        source: &MarkdownLayoutTextSource,
        level: u8,
    ) -> Result<Self> {
        source.health()?;
        if !request.accepts(&response)
            || request.kind != super::super::MarkdownLineInlineControlKind::EmptyParagraph
            || request.source_utf16 != (source.utf16_len()..source.utf16_len())
            || !source.permits(request.source())
        {
            return Err(Error::SourceMismatch);
        }
        let paragraph = source
            .snapshot_for_bidi()
            .text_checking_paragraph_range(request.source_utf16.clone())?;
        if paragraph.end != source.utf16_len() || level > 1 {
            return Err(Error::InconsistentCoverage);
        }
        let beginning = request.source_utf16.start == paragraph.start;
        let facts = MarkdownLineItemFacts {
            source_utf16: request.source_utf16.clone(),
            paragraph_utf16: paragraph,
            script: *b"Zyyy",
            level,
            direction: if level == 0 {
                crate::MarkdownShapingDirection::LeftToRight
            } else {
                crate::MarkdownShapingDirection::RightToLeft
            },
            points: response.points,
            beginning_of_text: beginning,
            end_of_text: true,
        };
        Ok(Self::from_response(response, facts))
    }
}
