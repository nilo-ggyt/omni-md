//! Only an actual producer-certified word prefix may use a nonnegative suffix bound. All candidate
//! boundaries still require the original grapheme and UAX14 indexes.
use super::*;
use crate::{MarkdownGlyphCluster, MarkdownGraphemeAnswer, MarkdownGraphemeQuery};

/// Retain one actual cluster while its original boundary queries yield; a pending query neither
/// commits width nor grants a safe line boundary.
pub(super) struct WordClusterProbe {
    cluster: MarkdownGlyphCluster,
    grapheme_checked: bool,
    cursor: Option<MarkdownLayoutBreakCursor>,
    shaping_seam: bool,
}

/// Retain one safe GCB candidate and the latest fitting GCB endpoint for the line. Continuing actual
/// prefixes retains neither old shaping jobs nor an array of proofs for every item.
pub(super) struct WordSafeState {
    word_end: usize,
    font: MarkdownLayoutFontAuthority,
    start_safe: bool,
    first_end: Option<usize>,
    latest_fitting_end: Option<usize>,
    fitting: Option<WordSafeFit>,
    incomplete_grapheme: bool,
}

struct WordSafeFit {
    end: usize,
    width: i64,
    flags: u32,
}

fn emergency(start: usize, fit: WordSafeFit, overflowing: bool) -> MarkdownSoftLineMeasureChoice {
    MarkdownSoftLineMeasureChoice::MeasuredEmergencyBreak {
        source_utf16: start..fit.end,
        x_advance_26_6: fit.width,
        boundary_flags: fit.flags,
        overflowing_grapheme: overflowing,
    }
}

impl MarkdownSoftLineMeasureJob {
    /// Return true after exhausting this quantum, awaiting an actual dependency, or selecting an
    /// answer. False returns control to the original natural-opportunity state machine.
    pub(super) fn advance_word_frontier(
        &mut self,
        provider: &mut dyn GlyphProvider,
        breaks: &mut MarkdownLayoutBreakJob,
        budget: MarkdownSoftLineMeasureBudget,
        step: &mut MarkdownSoftLineMeasureStep,
    ) -> Result<bool> {
        let Some(word_end) = provider.frontier_word_end(&self.source, &self.font)? else {
            return Ok(false);
        };
        let payload = self
            .payload
            .as_mut()
            .ok_or(MarkdownSoftLineMeasureError::Poisoned)?;
        if payload
            .candidate
            .is_some_and(|boundary| boundary.utf16 == payload.scan)
        {
            // A natural opportunity may be exactly at the previous actual shaping part's end.
            // Its successor supplies the missing cluster flags; return to the original measure
            // algorithm first. Scanning the long word now would lose this reached boundary/width
            // and incorrectly request reshaping across parts.
            return Ok(false);
        }
        if word_end < self.item.end || word_end > payload.source.utf16_len() {
            return Err(MarkdownSoftLineMeasureError::InconsistentCoverage);
        } // A final part may reach actual word end; that coordinate grants no glyph reads outside this provider.
        if payload
            .word_safe
            .as_ref()
            .is_none_or(|state| state.word_end != word_end || !state.font.same_payload(&self.font))
        {
            let incomplete_grapheme = payload
                .word_safe
                .as_ref()
                .is_some_and(|state| state.incomplete_grapheme);
            payload.word_safe = Some(WordSafeState {
                word_end,
                font: self.font.clone(),
                start_safe: false,
                first_end: None,
                latest_fitting_end: None,
                fitting: None,
                incomplete_grapheme,
            }); // Source stays fixed; changing font/word scope revokes prior emergency candidates rather than reauthorizing them across glyph dependencies.
        }
        loop {
            self.guard_provider(provider)?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownSoftLineMeasureError::Poisoned)?;
            if payload.scan == self.item.end {
                payload.selected = Some(MarkdownSoftLineMeasureChoice::NeedNextItem {
                    source_utf16: self.start..self.item.end,
                    x_advance_26_6: payload.advance,
                    last_fitting: payload.fitting.as_ref().map(|fit| fit.measured.clone()),
                });
                step.answer = MarkdownSoftLineMeasureAnswer::Selected(
                    payload
                        .selected
                        .clone()
                        .ok_or(MarkdownSoftLineMeasureError::Poisoned)?,
                );
                return Ok(true); // Request the next item only after actual prefix consumption; word-end coordinates are not glyph EOF.
            }
            if payload.word_probe.is_none() {
                // Cluster and boundary-query budgets are independent capped dimensions. Preserve
                // an in-flight probe when either dimension yields; neither is a wall-time/RSS bound.
                if step.clusters == budget.clusters.min(MARKDOWN_SOFT_LINE_CLUSTERS) {
                    return Ok(true);
                }
                step.clusters += 1;
                let cluster = match provider.cluster(&self.source, &self.font, payload.scan)? {
                    MarkdownGlyphClusterAnswer::Pending => {
                        step.answer = MarkdownSoftLineMeasureAnswer::PendingDependencies;
                        return Ok(true);
                    }
                    MarkdownGlyphClusterAnswer::Known(Some(value)) => value,
                    MarkdownGlyphClusterAnswer::Known(None) => {
                        return Err(MarkdownSoftLineMeasureError::InconsistentCoverage);
                    }
                };
                if cluster.source_utf16.start != payload.scan
                    || cluster.source_utf16.end <= payload.scan
                    || cluster.source_utf16.end > self.item.end
                {
                    return Err(MarkdownSoftLineMeasureError::InconsistentCoverage);
                }
                if cluster.missing_glyphs != 0 {
                    payload.selected = Some(MarkdownSoftLineMeasureChoice::NeedsFontFallback {
                        cluster_utf16: cluster.source_utf16,
                        missing_glyphs: cluster.missing_glyphs,
                    });
                    step.answer = MarkdownSoftLineMeasureAnswer::Selected(
                        payload
                            .selected
                            .clone()
                            .ok_or(MarkdownSoftLineMeasureError::Poisoned)?,
                    );
                    return Ok(true); // Actual glyphs must agree with certified closure; .notdef width cannot authorize an emergency line.
                }
                let safe = payload
                    .word_safe
                    .as_mut()
                    .ok_or(MarkdownSoftLineMeasureError::Poisoned)?;
                if payload.scan == self.start {
                    safe.start_safe = cluster.flags & UNSAFE_TO_BREAK == 0;
                } else if safe.start_safe
                    && !safe.incomplete_grapheme
                    && cluster.flags & UNSAFE_TO_BREAK == 0
                {
                    // This actual cluster succeeds the previous GCB. Its own flags establish
                    // whether that cut can reuse glyphs; aggregate prefix flags, item endpoints,
                    // or a missing successor record cannot establish safety.
                    let fit = WordSafeFit {
                        end: payload.scan,
                        width: payload.advance,
                        flags: cluster.flags,
                    };
                    if fit.width <= self.width {
                        safe.fitting = Some(fit);
                    } else if safe.fitting.is_none() && safe.first_end == Some(fit.end) {
                        payload.selected = Some(emergency(self.start, fit, true));
                        step.answer = MarkdownSoftLineMeasureAnswer::Selected(
                            payload
                                .selected
                                .clone()
                                .ok_or(MarkdownSoftLineMeasureError::Poisoned)?,
                        );
                        return Ok(true); // Preserve an overflowing first complete GCB; never split it or pretend it fits.
                    }
                }
                payload.word_probe = Some(WordClusterProbe {
                    cluster,
                    grapheme_checked: false,
                    cursor: None,
                    shaping_seam: false,
                });
            }
            let probe = payload
                .word_probe
                .as_mut()
                .ok_or(MarkdownSoftLineMeasureError::Poisoned)?;
            let end = probe.cluster.source_utf16.end;
            if !probe.grapheme_checked {
                if step.break_queries == budget.break_queries.min(MARKDOWN_SOFT_LINE_BREAK_QUERIES)
                {
                    return Ok(true);
                }
                step.break_queries += 1;
                match breaks.grapheme_query(&self.source, end, MarkdownGraphemeQuery::IsBoundary)? {
                    MarkdownGraphemeAnswer::Pending => {
                        step.answer = MarkdownSoftLineMeasureAnswer::PendingDependencies;
                        return Ok(true);
                    }
                    MarkdownGraphemeAnswer::Known(Some(boundary)) if boundary.utf16 == end => {}
                    MarkdownGraphemeAnswer::Known(None)
                        if end == self.item.end
                            && provider.bidi_shaping_end(&self.source, &self.font)?
                                == Some(end) =>
                    {
                        probe.shaping_seam = true;
                    }
                    MarkdownGraphemeAnswer::Known(_) => {
                        return Err(MarkdownSoftLineMeasureError::InconsistentCoverage);
                    }
                }
                probe.grapheme_checked = true; // The immutable original index lets quota-one continuations retain this completed check.
            }
            if probe.shaping_seam {
                let probe = payload
                    .word_probe
                    .take()
                    .ok_or(MarkdownSoftLineMeasureError::Poisoned)?;
                payload.advance = payload
                    .advance
                    .checked_add(probe.cluster.x_advance_26_6)
                    .ok_or(MarkdownSoftLineMeasureError::Overflow)?;
                payload.scan = end;
                payload
                    .word_safe
                    .as_mut()
                    .ok_or(MarkdownSoftLineMeasureError::Poisoned)?
                    .incomplete_grapheme = true;
                continue; // Accumulate only this actual shaping fragment; its interior seam cannot update fitting/first-GCB/emergency candidates.
            }
            if step.break_queries == budget.break_queries.min(MARKDOWN_SOFT_LINE_BREAK_QUERIES) {
                return Ok(true);
            }
            if probe.cursor.is_none() {
                // Keep the original At-query cursor across yields after the GCB check. Unknown
                // source dependencies do not become Allowed, Mandatory, or an empty answer.
                probe.cursor = Some(breaks.cursor(
                    MarkdownLayoutBreakUnit::Utf16,
                    end,
                    MarkdownLayoutBreakQuery::At,
                )?);
            }
            step.break_queries += 1;
            let boundary = match breaks.query(
                probe
                    .cursor
                    .as_mut()
                    .ok_or(MarkdownSoftLineMeasureError::Poisoned)?,
                1,
            )? {
                MarkdownLayoutBreakAnswer::PendingSource => {
                    step.answer = MarkdownSoftLineMeasureAnswer::PendingDependencies;
                    return Ok(true);
                }
                MarkdownLayoutBreakAnswer::Yielded => return Ok(true),
                MarkdownLayoutBreakAnswer::Known(value) => value,
            };
            let probe = payload
                .word_probe
                .take()
                .ok_or(MarkdownSoftLineMeasureError::Poisoned)?;
            payload.advance = payload
                .advance
                .checked_add(probe.cluster.x_advance_26_6)
                .ok_or(MarkdownSoftLineMeasureError::Overflow)?;
            payload.scan = end;
            let safe = payload
                .word_safe
                .as_mut()
                .ok_or(MarkdownSoftLineMeasureError::Poisoned)?;
            safe.incomplete_grapheme = false; // Actual outer GCB/At checks completed, so line-boundary candidates may resume.
            if safe.first_end.is_none() {
                safe.first_end = Some(end);
            } // Record the first endpoint only after actual GCB/At completion, never during budget suspension.
            if payload.advance <= self.width {
                // Keep the latest actual fitting endpoint even if its cut depends on later kerning.
                // An earlier reusable cut cannot replace it merely to avoid reshaping. Direct reuse
                // requires the same endpoint; otherwise retain actual prefix reshaping.
                safe.latest_fitting_end = Some(end);
            }
            if let Some(boundary) = boundary {
                if boundary.utf16 != end || payload.candidate.is_some_and(|old| old.utf16 != end) {
                    return Err(MarkdownSoftLineMeasureError::InconsistentCoverage);
                }
                payload.candidate = Some(boundary);
                payload.cursor = None;
                return Ok(false); // Return actual UAX opportunities to the original algorithm; invent no Allowed/Mandatory/Dictionary answer.
            }
            if payload.advance > self.width {
                payload.selected = Some(match payload.fitting.take() {
                    Some(fitting) => selected(fitting),
                    None => match safe
                        .fitting
                        .take()
                        .filter(|fit| Some(fit.end) == safe.latest_fitting_end)
                    {
                        Some(fitting) => emergency(self.start, fitting, false),
                        None if safe.first_end == Some(end) => {
                            continue; // Await actual successor flags for the first GCB before authorizing overflow in a later quantum/prefix.
                        }
                        None => MarkdownSoftLineMeasureChoice::NeedsFrontierEmergencyPrefix {
                            source_utf16: self.start..self.item.end,
                            word_end_utf16: word_end,
                            measured_prefix_26_6: payload.advance,
                        },
                    },
                });
                step.answer = MarkdownSoftLineMeasureAnswer::Selected(
                    payload
                        .selected
                        .clone()
                        .ok_or(MarkdownSoftLineMeasureError::Poisoned)?,
                );
                return Ok(true); // Prior GCBs were actual NoBreak positions, and the unread same-font word suffix has a certified nonnegative bound.
            }
        }
    }
}
