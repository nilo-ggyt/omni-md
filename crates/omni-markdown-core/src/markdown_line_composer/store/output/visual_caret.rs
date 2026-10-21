//! Move one real GCB inside the original visual part; a part edge grants no line or document edge authority.

use super::*;

mod line;
pub use line::{
    MarkdownComposedVisualAnswer, MarkdownComposedVisualQuery, MarkdownComposedVisualStep,
};
#[cfg(feature = "paged-shaping-frontier")]
mod word;
#[cfg(feature = "paged-shaping-frontier")]
pub use word::{
    MarkdownComposedVisualWordAnswer, MarkdownComposedVisualWordQuery,
    MarkdownComposedVisualWordStep,
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

/// External carets require an actual GCB. Entering an authenticated visual part
/// from the whole-line query may also skip a shaping endpoint inside a GCB.
#[derive(Clone, Copy)]
enum GraphemeQueryStart {
    Caret,
    PartEntry { inclusive: bool },
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
    folded_section: bool,
    bytes: usize,
    units: usize,
    search: graphemes::BoundarySearch,
    locating: bool,
    start: GraphemeQueryStart,
    #[cfg(feature = "paged-shaping-frontier")]
    origin: Option<(usize, crate::MarkdownGraphemeBoundary)>,
    #[cfg(feature = "paged-shaping-frontier")]
    neighbor: Option<(usize, crate::MarkdownGraphemeBoundary)>,
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
            let folded_section =
                part.facts.kind == crate::MarkdownComposedLinePartKind::FoldedSection;
            if folded_section && utf16 != range.start && utf16 != range.end {
                return Err(Error::InvalidRequest);
            } // Fold controls expose only two endpoints; hidden Source GCBs grant no caret authority.
            let logical_forward = match part.facts.direction {
                // Convert the requested visual side using this actual part's
                // original resolved direction, not a guessed paragraph direction.
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
                folded_section,
                bytes: source.byte_len(),
                units: source.utf16_len(),
                search: graphemes::BoundarySearch::offset(utf16),
                locating: true,
                start: GraphemeQueryStart::Caret,
                #[cfg(feature = "paged-shaping-frontier")]
                origin: None,
                #[cfg(feature = "paged-shaping-frontier")]
                neighbor: None,
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
                    let exact = value.filter(|value| value.utf16 == query.utf16);
                    if let GraphemeQueryStart::PartEntry { inclusive } = query.start
                        && (inclusive || exact.is_none())
                    {
                        // lower_bound supplies the real successor. Entering an
                        // RTL/LTR part toward logical start selects its predecessor
                        // instead; an internal scalar seam is never a caret.
                        let ordinal = if query.logical_forward || exact.is_some() {
                            ordinal
                        } else {
                            ordinal.checked_sub(1).ok_or(Error::InconsistentCoverage)?
                        };
                        query.search = graphemes::BoundarySearch::ordinal(ordinal);
                        query.locating = false;
                        continue;
                    }
                    let boundary = exact.ok_or(Error::InvalidRequest)?;
                    #[cfg(feature = "paged-shaping-frontier")]
                    {
                        query.origin = Some((ordinal, boundary));
                    } // Keep only the directory's actual byte/UTF-16 pair; Bidi may choose an equivalent caret side.
                    let edge = if query.logical_forward {
                        query.range.end
                    } else {
                        query.range.start
                    };
                    if boundary.utf16 == edge {
                        query.answer = Some(MarkdownComposedVisualGraphemeAnswer::Edge);
                        break;
                    }
                    if query.folded_section {
                        // Query this same output's real endpoint GCB. A whole
                        // hidden range is not one ordinary character whose
                        // internal ordinal would authorize another caret.
                        query.search = graphemes::BoundarySearch::offset(edge);
                    } else {
                        let ordinal = if query.logical_forward {
                            ordinal.checked_add(1).ok_or(Error::Overflow)?
                        } else {
                            ordinal.checked_sub(1).ok_or(Error::InconsistentCoverage)?
                        };
                        query.search = graphemes::BoundarySearch::ordinal(ordinal);
                    }
                    query.locating = false;
                    continue; // The next ordinal still consumes the remaining budget of this same turn.
                }
                let boundary = value.ok_or(Error::InconsistentCoverage)?;
                let next = boundary.utf16;
                #[cfg(feature = "paged-shaping-frontier")]
                {
                    query.neighbor = Some((ordinal, boundary));
                }
                if next < query.range.start || next > query.range.end {
                    // A real GCB may span UBA/font parts. This part cannot issue
                    // a cross-domain caret; the original whole-line query finds
                    // its neighbor, preserving the noneditable internal seam.
                    query.answer = Some(MarkdownComposedVisualGraphemeAnswer::Edge);
                    break;
                }
                if (query.folded_section
                    && next
                        != if query.logical_forward {
                            query.range.end
                        } else {
                            query.range.start
                        })
                    || (matches!(query.start, GraphemeQueryStart::Caret)
                        && ((query.logical_forward && next <= query.utf16)
                            || (!query.logical_forward && next >= query.utf16)))
                {
                    // Fold traversal must reach its exact opposite endpoint;
                    // ordinary carets must advance strictly in logical direction.
                    return Err(Error::InconsistentCoverage);
                }
                query.answer = Some(MarkdownComposedVisualGraphemeAnswer::Found {
                    utf16: next,
                    side: MarkdownComposedCaretSide {
                        level: query.level,
                        association: if matches!(query.start, GraphemeQueryStart::PartEntry { .. })
                            && next == query.range.start
                        {
                            // Entering at logical start associates with this
                            // part's following grapheme, regardless of bidi level.
                            MarkdownComposedCaretAssociation::After
                        } else if matches!(query.start, GraphemeQueryStart::PartEntry { .. })
                            && next == query.range.end
                            || query.logical_forward
                        {
                            // Logical end/forward movement retains the side of
                            // the preceding actual grapheme at the destination.
                            MarkdownComposedCaretAssociation::Before
                        } else {
                            // Reverse movement retains the following grapheme's
                            // side; the whole-line layer handles adjacent parts.
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
