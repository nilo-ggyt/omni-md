//! Carry the original response after validating its nonce, actual grapheme
//! boundaries, and paragraph extent; this value stays inside its original worker.
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
    /// Revalidate the same Source, complete control interval, actual registered
    /// font, and bounded paragraph facts before consuming derived geometry.
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
            // A mismatched source/extent/size cannot borrow the old request's
            // authority. Table rows alone legitimately carry no text point size.
            return Err(Error::SourceMismatch);
        }
        let actual = fonts.require_registration(&self.registered.registration())?;
        if !actual.font().permits(&self.registered.font().authority()) {
            // Registration coordinates cannot replace exact live font payload identity.
            return Err(Error::SourceMismatch);
        }
        self.paragraph.require()?;
        // Cache hits, EOF, and successful font registration do not bypass an
        // original source failure that arrived during these metadata checks.
        source.health().map_err(Error::from)
    }
    /// Transfer only the actually accepted response and its matching Core facts;
    /// no native object or replacement Source is constructed here.
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
    /// Construct the real empty document or paragraph after an actual trailing
    /// separator, requiring the same worker's private EOF-control nonce.
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
            // A zero-width interval elsewhere cannot masquerade as the actual
            // empty EOF paragraph or reuse another worker's outstanding control.
            return Err(Error::SourceMismatch);
        }
        let paragraph = source
            .snapshot_for_bidi()
            .text_checking_paragraph_range(request.source_utf16.clone())?;
        if paragraph.end != source.utf16_len() || level > 1 {
            // The completed empty paragraph must end at real EOF and carry a
            // valid paragraph base level, never an invented interior embedding level.
            return Err(Error::InconsistentCoverage);
        }
        let beginning = request.source_utf16.start == paragraph.start;
        let facts = MarkdownLineItemFacts {
            source_utf16: request.source_utf16.clone(),
            paragraph_utf16: paragraph,
            script: *b"Zyyy",
            level,
            direction: if level == 0 {
                // The real paragraph base level determines the control's direction.
                crate::MarkdownShapingDirection::LeftToRight
            } else {
                // Level one is the admitted right-to-left empty paragraph base.
                crate::MarkdownShapingDirection::RightToLeft
            },
            points: response.points,
            beginning_of_text: beginning,
            end_of_text: true,
        };
        Ok(Self::from_response(response, facts))
    }
}
