//! Bind actual style responses for nonglyph controls. Paragraph changes preserve
//! source bytes, and tabs use real stops rather than a fixed number of spaces.
use super::{MarkdownLineComposeError as Error, MarkdownLineRegisteredFont, item::Result};
use crate::MarkdownLayoutTextAuthority;
mod validated;
use std::{ops::Range, sync::Arc};
pub(super) use validated::ValidatedControl;

/// Alignment captured from the original platform paragraph style. Nonleft
/// alignment needs actual following-content width/terminators before completion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownLineTabAlignment {
    Left,
    Right,
    Center,
    Decimal,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// One actual tab stop in signed 26.6 layout coordinates, with its captured alignment.
pub struct MarkdownLineTabStop {
    pub location_26_6: i64,
    pub alignment: MarkdownLineTabAlignment,
}

/// Bounded paragraph metadata. None explicitly means the system's default
/// interval is not yet known, without guessing 28 points or eight spaces.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownLineParagraphStyle {
    pub tab_stops: Vec<MarkdownLineTabStop>,
    pub default_tab_interval_26_6: Option<i64>,
}
impl MarkdownLineParagraphStyle {
    /// Admit only bounded, ordered nonnegative stop metadata; never truncate the
    /// platform's actual stops or turn malformed style into a default layout.
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
            // Reject invalid metadata before tab lookahead or arithmetic can
            // turn it into a partially accepted line geometry.
            return Err(Error::InvalidRequest);
        }
        Ok(())
    }
    /// Resolve the next actual stop, or request the missing interval/lookahead
    /// needed to preserve the paragraph's real alignment policy.
    pub(super) fn tab(&self, pen: i64) -> Result<MarkdownLineTabPlacement> {
        self.require()?;
        if let Some(stop) = self.tab_stops.iter().find(|stop| stop.location_26_6 > pen) {
            // A declared stop strictly ahead of the current pen has priority
            // over any implicit default grid.
            if stop.alignment != MarkdownLineTabAlignment::Left {
                // Right/center/decimal geometry is incomplete until following
                // content has been measured; this is an explicit pending result.
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
            // Unknown platform defaults remain pending rather than guessed spacing.
            return Ok(MarkdownLineTabPlacement::NeedsDefaultInterval);
        };
        if interval == 0 {
            // A known zero interval advances no further after explicit stops;
            // it never moves a pen already beyond the last stop backward.
            return Ok(MarkdownLineTabPlacement::Left { advance_26_6: 0 });
        }
        // Actual Cocoa comparison anchors the implicit grid at the last explicit
        // stop, rather than at document x=0; retain signed checked 26.6 arithmetic.
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

/// Right/center/decimal alignment remains pending until real lookahead exists;
/// this geometry result grants neither source-range nor drawing authority.
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

/// Nonglyph parts still retain actual Source/font/grapheme facts. Skipping ink
/// does not permit native consumers to skip geometry or selection semantics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownComposedLinePartKind {
    Glyphs,
    Tab,
    ParagraphBreak,
    /// The canonical hidden interval covered by an original fold control has
    /// no internal visible glyphs or hittable caret positions.
    FoldedSection,
    /// A table block row with actual cell layout at final column widths,
    /// without body glyph output at this outer block level.
    TableRow,
}
impl MarkdownComposedLinePartKind {
    /// Encode the declared part kind in the same private immutable output format.
    pub(super) fn code(self) -> u8 {
        match self {
            Self::Glyphs => 0,
            Self::Tab => 1,
            Self::ParagraphBreak => 2,
            Self::FoldedSection => 3,
            Self::TableRow => 4,
        }
    }
    /// Reject unknown stored kinds instead of guessing glyph geometry for them.
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

/// Retain one actually registered font and bounded paragraph style, never native
/// objects, a document/history owner, or a second content representation.
pub struct MarkdownLineControlResponse {
    pub(super) request: super::MarkdownLineInlineControl,
    pub(super) registered: MarkdownLineRegisteredFont,
    pub(super) points: f32,
    pub(super) paragraph: MarkdownLineParagraphStyle,
    pub(super) presentation: crate::MarkdownPresentationStyles,
    pub(super) paragraph_inset_26_6: i32,
}
impl MarkdownLineControlResponse {
    /// Apply the same source semantics to controls. A break paints no ink, but
    /// its font/paragraph geometry must not restore height for a hidden fence marker.
    pub fn with_presentation(
        mut self,
        styles: crate::MarkdownPresentationStyles,
        inset_26_6: i32,
    ) -> Result<Self> {
        if inset_26_6 < 0 {
            // Negative inset cannot become a valid later available-width calculation.
            return Err(Error::InvalidRequest);
        }
        self.presentation = styles;
        self.paragraph_inset_26_6 = inset_26_6;
        Ok(self)
    }
}
impl super::MarkdownLineInlineControl {
    /// Return the original control nonce and source ticket with the response;
    /// editing public facts cannot substitute for the actual outstanding request.
    pub fn respond(
        self,
        registered: &MarkdownLineRegisteredFont,
        points: f32,
        paragraph: MarkdownLineParagraphStyle,
    ) -> Result<MarkdownLineControlResponse> {
        paragraph.require()?;
        if !points.is_finite() || points <= 0.0 {
            // Invalid point sizes cannot produce a usable control's font geometry.
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
    /// Borrow the original weak Source authority, without recapturing current text.
    pub fn source(&self) -> &MarkdownLayoutTextAuthority {
        &self.source
    }
    /// Bind one private request nonce to its actual source interval and control role.
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
    /// Require the original nonce, source, range, and role together; identical
    /// coordinates from a newer control cannot authorize this response.
    pub(super) fn accepts(&self, response: &MarkdownLineControlResponse) -> bool {
        Arc::ptr_eq(&self.nonce, &response.request.nonce)
            && self.source == response.request.source
            && self.source_utf16 == response.request.source_utf16
            && self.kind == response.request.kind
    }
}

#[cfg(test)]
mod tests;
