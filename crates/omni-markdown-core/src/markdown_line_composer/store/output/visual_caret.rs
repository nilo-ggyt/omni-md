//! Move one real GCB inside the original visual part; a part edge grants no line or document edge authority.

use super::*;

mod line;
pub use line::{
    MarkdownComposedVisualAnswer, MarkdownComposedVisualQuery, MarkdownComposedVisualStep,
};

/// The logical grapheme before/after this position, independent of upstream/downstream soft-wrap affinity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownComposedCaretAssociation {
    Before,
    After,
}

/// A side from actual L1/L2 parts; callers must still authenticate Source, input serial and layout geometry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownComposedCaretSide {
    pub level: u8,
    pub association: MarkdownComposedCaretAssociation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownComposedVisualGraphemeAnswer {
    Pending,
    /// Only this part has ended; an adjacent part or line must come from the same real output.
    Edge,
    Found {
        utf16: usize,
        side: MarkdownComposedCaretSide,
    },
}

/// A weak part/directory query retaining one GCB record across suspension, without a reader or text.
pub struct MarkdownComposedVisualGraphemeQuery {
    output: Weak<()>,
    source: MarkdownLayoutTextAuthority,
    registration: MarkdownLineFontRegistration,
    range: Range<usize>,
    level: u8,
    utf16: usize,
    logical_forward: bool,
    bytes: usize,
    units: usize,
    search: graphemes::BoundarySearch,
    locating: bool,
    answer: Option<MarkdownComposedVisualGraphemeAnswer>,
    failed: bool,
}

pub struct MarkdownComposedVisualGraphemeStep {
    pub records: usize,
    pub answer: MarkdownComposedVisualGraphemeAnswer,
}

impl MarkdownComposedLines {
    /// Follow this authenticated part direction for one GCB; do not infer adjacency across font or UBA boundaries.
    ///
    /// # Errors
    /// Foreign parts, Source/font errors, cancellation and directory IO retain the original failure and revoke output;
    /// a non-GCB or out-of-part position rejects this input without poisoning a healthy output.
    pub fn visual_grapheme_query(
        &mut self,
        part: &MarkdownComposedLinePart,
        utf16: usize,
        right: bool,
    ) -> Result<MarkdownComposedVisualGraphemeQuery> {
        let result = (|| {
            self.require(&part.source)?;
            if !Weak::ptr_eq(&part.output, &Arc::downgrade(&self.identity)) {
                return Err(Error::SourceMismatch);
            }
            self.payload
                .as_ref()
                .ok_or(Error::Poisoned)?
                .fonts
                .require_registration(&part.registration)?;
            let range = &part.facts.source_utf16;
            if utf16 < range.start || utf16 > range.end {
                return Err(Error::InvalidRequest);
            }
            let logical_forward = match part.facts.direction {
                MarkdownShapingDirection::LeftToRight => right,
                MarkdownShapingDirection::RightToLeft => !right,
                _ => return Err(Error::InconsistentCoverage),
            };
            let source = &self.payload.as_ref().ok_or(Error::Poisoned)?.source;
            Ok(MarkdownComposedVisualGraphemeQuery {
                output: part.output.clone(),
                source: part.source.clone(),
                registration: part.registration.clone(),
                range: range.clone(),
                level: part.facts.level,
                utf16,
                logical_forward,
                bytes: source.byte_len(),
                units: source.utf16_len(),
                search: graphemes::BoundarySearch::offset(utf16),
                locating: true,
                answer: None,
                failed: false,
            })
        })();
        if matches!(result, Err(Error::InvalidRequest)) {
            self.require(&part.source)?;
            return result; // Invalid external positions reject this action without poisoning the healthy Source/reader.
        }
        self.finish(result)
    }

    /// Advance the same part under one record budget; quota one may suspend at any original link.
    pub fn step_visual_grapheme(
        &mut self,
        query: &mut MarkdownComposedVisualGraphemeQuery,
        budget: usize,
    ) -> Result<MarkdownComposedVisualGraphemeStep> {
        let result = (|| {
            self.require(&query.source)?;
            if query.failed {
                return Err(Error::Poisoned);
            }
            if !Weak::ptr_eq(&query.output, &Arc::downgrade(&self.identity)) {
                return Err(Error::SourceMismatch);
            }
            self.payload
                .as_ref()
                .ok_or(Error::Poisoned)?
                .fonts
                .require_registration(&query.registration)?;
            if budget == 0 || budget > STEP {
                return Err(Error::InvalidRequest);
            }
            let mut records = 0;
            while query.answer.is_none() && records < budget {
                if query.utf16 > self.graphemes.covered.utf16 {
                    break; // This snapshot lacks the GCB; only the original Producer can publish a later prefix.
                }
                let step = query.search.step(
                    self.graphemes,
                    query.bytes,
                    query.units,
                    budget - records,
                    |record| self.row(record),
                )?;
                records += step.records;
                let Some((ordinal, value)) = step.answer else {
                    break;
                };
                if query.locating {
                    let boundary = value
                        .filter(|value| value.utf16 == query.utf16)
                        .ok_or(Error::InvalidRequest)?;
                    let edge = if query.logical_forward {
                        query.range.end
                    } else {
                        query.range.start
                    };
                    if boundary.utf16 == edge {
                        query.answer = Some(MarkdownComposedVisualGraphemeAnswer::Edge);
                        break;
                    }
                    let ordinal = if query.logical_forward {
                        ordinal.checked_add(1).ok_or(Error::Overflow)?
                    } else {
                        ordinal.checked_sub(1).ok_or(Error::InconsistentCoverage)?
                    };
                    query.search = graphemes::BoundarySearch::ordinal(ordinal);
                    query.locating = false;
                    continue; // The next ordinal still consumes the remaining budget of this same turn.
                }
                let next = value.ok_or(Error::InconsistentCoverage)?.utf16;
                if next < query.range.start
                    || next > query.range.end
                    || (query.logical_forward && next <= query.utf16)
                    || (!query.logical_forward && next >= query.utf16)
                {
                    return Err(Error::InconsistentCoverage);
                }
                query.answer = Some(MarkdownComposedVisualGraphemeAnswer::Found {
                    utf16: next,
                    side: MarkdownComposedCaretSide {
                        level: query.level,
                        association: if query.logical_forward {
                            MarkdownComposedCaretAssociation::Before
                        } else {
                            MarkdownComposedCaretAssociation::After
                        },
                    },
                });
            }
            self.require(&query.source)?;
            self.payload
                .as_ref()
                .ok_or(Error::Poisoned)?
                .fonts
                .require_registration(&query.registration)?;
            Ok(MarkdownComposedVisualGraphemeStep {
                records,
                answer: query
                    .answer
                    .unwrap_or(MarkdownComposedVisualGraphemeAnswer::Pending),
            })
        })();
        if result.is_err() {
            query.failed = true;
        }
        if matches!(result, Err(Error::InvalidRequest)) {
            self.require(&query.source)?;
            return result; // Reject this query only; do not poison healthy output or permit changing the rejected query position.
        }
        self.finish(result)
    }
}
