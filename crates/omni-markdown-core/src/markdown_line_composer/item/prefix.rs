//! Join original stable script/grapheme prefixes on demand. An open interval
//! cannot substitute for real paragraph authority or completed dependencies.
use super::*;
use crate::markdown_scripts::{MarkdownScriptPrefixAnswer, MarkdownScriptPrefixKind};

/// Permit an open script query only through complete uniform control-free proof;
/// grapheme and font authority still require their own independent checks.
pub(super) fn unicode_prefix_allowed(
    payload: &Payload,
    authority: &MarkdownLayoutTextAuthority,
) -> Result<bool> {
    if !payload.frontier_enabled {
        // Nonfrontier composition retains its existing complete dependency contract.
        return Ok(false);
    }
    let bidi = payload
        .bidi
        .as_ref()
        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
    let end = payload
        .cursor
        .saturating_add(1)
        .min(payload.source.utf16_len());
    Ok(bidi
        .uniform_control_free_span(authority, payload.cursor..end)?
        .is_some())
}

/// Retain one finite coordinate probe per start. Each metadata query occupies
/// its own quantum, including when a one-unit budget cannot pay for a later query.
pub(super) struct OpenPrefixProbe {
    run: crate::MarkdownScriptRun,
    // For prepared multi-paragraph input, accumulate through the original closed
    // item allowance or real bidi endpoint before repeatedly probing fonts on tiny prefixes.
    through: usize,
    paragraph_end: Option<usize>,
    end: Option<crate::MarkdownGraphemeBoundary>,
    first: Option<crate::MarkdownGraphemeBoundary>,
    phase: u8,
}

/// Return original script facts while retaining open/closed scope separately.
/// Unknown coverage requests the original dependency's next step, not a substring scan.
pub(super) fn script_at(
    payload: &mut Payload,
    authority: &MarkdownLayoutTextAuthority,
) -> Result<Option<crate::MarkdownScriptRun>> {
    payload.open_script = false;
    let allowed = unicode_prefix_allowed(payload, authority)?;
    if allowed && payload.closed_script_at == Some(payload.cursor) {
        let span = payload
            .bidi
            .as_ref()
            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?
            .uniform_control_free_span(
                authority,
                payload.cursor..payload.cursor.saturating_add(1),
            )?;
        if let Some(span) = span {
            // The shaping plan requested actual closure. Reuse the original Latin
            // suffix proof through this Source's bidi content end; non-EOF B/CRLF
            // still closes naturally rather than treating a 256-unit preparation cap as SCX end.
            payload.scripts.prepare_ascii_suffix(
                authority,
                payload.cursor,
                span.content_end(),
                span.paragraph_range().end,
            )?;
        }
    }
    if !allowed || payload.closed_script_at == Some(payload.cursor) {
        // Without open-prefix authority, consume only the actual complete run;
        // pending data requests source work without advancing item coordinates.
        return match payload
            .scripts
            .run_at(authority, MarkdownScriptUnit::Utf16, payload.cursor)?
        {
            MarkdownScriptAnswer::Known(Some(run)) => {
                // A genuinely completed run consumes the one pending close request.
                payload.closed_script_at = None;
                Ok(Some(run))
            }
            MarkdownScriptAnswer::Pending => {
                // Keep the cursor unchanged and let a separately charged script step progress.
                payload.script_source_demand = true;
                Ok(None)
            }
            // A missing run at a nonterminal item position is inconsistent coverage.
            _ => Err(MarkdownLineComposeError::InconsistentCoverage),
        };
    }
    if payload.open_probe.is_none() {
        // Obtain only one original stable-prefix fact before any grapheme query;
        // the following phases retain that exact run rather than recapturing current state.
        let (run, open) = match payload.scripts.stable_prefix_at(
            authority,
            MarkdownScriptUnit::Utf16,
            payload.cursor,
        )? {
            MarkdownScriptPrefixAnswer::Known(Some(prefix)) => (
                prefix.facts()?,
                prefix.kind()? == MarkdownScriptPrefixKind::StableOpenPrefix,
            ),
            MarkdownScriptPrefixAnswer::Pending => {
                // No stable original coverage exists yet; request the real script dependency.
                payload.script_source_demand = true;
                return Ok(None);
            }
            MarkdownScriptPrefixAnswer::Known(None) => {
                // The current nonterminal cursor requires a real script run.
                return Err(MarkdownLineComposeError::InconsistentCoverage);
            }
        };
        if !open {
            // Naturally closed script coverage needs no additional open-prefix probe.
            return Ok(Some(run));
        }
        let paragraph_end = if payload
            .bidi
            .as_ref()
            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?
            .uses_prepared_paragraphs()
        {
            payload
                .bidi
                .as_ref()
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?
                .uniform_control_free_span(
                    authority,
                    payload.cursor..payload.cursor.saturating_add(1),
                )?
                .map(|span| span.paragraph_range().end)
        } else {
            None
        };
        let through = paragraph_end
            .map(|end| end.min(payload.cursor.saturating_add(closed_item::MAX_CLOSED_UTF16)))
            .unwrap_or(payload.cursor);
        if run.end_utf16 < through {
            // The preparation target may need several original source quanta;
            // it grants neither a script boundary nor permission to probe fonts yet.
            payload.script_source_demand = true;
            return Ok(None);
        }
        payload.open_probe = Some(OpenPrefixProbe {
            run,
            through,
            paragraph_end,
            end: None,
            first: None,
            phase: 0,
        });
        // This quantum consumed only the original script directory. Grapheme
        // coverage must be queried in a subsequent independently charged step.
        return Ok(None);
    }
    let probe = payload
        .open_probe
        .as_mut()
        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
    match probe.phase {
        0 => {
            // Read committed original grapheme coverage and cap it by the exact
            // stable script run; neither directory may enlarge the other's proof.
            let facts = payload.breaks.grapheme_prefix_facts(authority)?;
            let Some(boundary) = facts
                .last_committed_boundary
                .filter(|value| value.utf16 > payload.cursor && value.utf16 >= probe.through)
            else {
                // Leave actual Next-demand generation to another quantum;
                // GB2 coordinates alone cannot manufacture a committed record.
                probe.phase = 1;
                return Ok(None);
            };
            probe.end = Some(if boundary.utf16 < probe.run.end_utf16 {
                boundary
            } else {
                crate::MarkdownGraphemeBoundary {
                    byte: probe.run.end_byte,
                    utf16: probe.run.end_utf16,
                }
            });
            probe.phase = 1;
        }
        1 => {
            // Obtain a real following grapheme, or demand enough original
            // grapheme coverage to reach the existing preparation target.
            if probe.end.is_none() && probe.through > payload.cursor {
                match payload.breaks.grapheme_query(
                    authority,
                    probe.through,
                    MarkdownGraphemeQuery::Next,
                )? {
                    MarkdownGraphemeAnswer::Known(_) => probe.phase = 0,
                    MarkdownGraphemeAnswer::Pending => {}
                }
                // Real Next may cross a long cluster or CRLF. Pending advances
                // only the original grapheme job, never certifies the target offset.
                return Ok(None);
            }
            match payload.breaks.grapheme_query(
                authority,
                payload.cursor,
                MarkdownGraphemeQuery::Next,
            )? {
                MarkdownGraphemeAnswer::Known(Some(value)) => {
                    probe.first = Some(value);
                    probe.phase = if probe.end.is_some() { 2 } else { 0 };
                }
                MarkdownGraphemeAnswer::Pending => {}
                _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
            }
        }
        2 => {
            // The original first and next grapheme establish a complete support
            // interval before publishing any shortened stable script prefix.
            let first = probe
                .first
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
            let second = match payload.breaks.grapheme_query(
                authority,
                first.utf16,
                MarkdownGraphemeQuery::Next,
            )? {
                MarkdownGraphemeAnswer::Known(Some(value)) => value,
                MarkdownGraphemeAnswer::Pending => return Ok(None),
                _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
            };
            let end = probe
                .end
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
            if second.utf16 > end.utf16 {
                // Script coverage is shorter than the complete support interval.
                // Advance its original job and reacquire proof without enlarging this run.
                payload.open_probe = None;
                payload.script_source_demand = true;
                return Ok(None);
            }
            let mut bounded = probe.run;
            bounded.end_utf16 = end.utf16;
            bounded.end_byte = end.byte;
            let open = probe
                .paragraph_end
                .is_none_or(|paragraph_end| bounded.end_utf16 < paragraph_end);
            payload.open_probe = None;
            // Stable SCX through the real bidi endpoint allows later natural
            // paragraph/control closure. Merely reaching the preparation allowance
            // stays open and grants no ClosedItem or font-boundary authority.
            payload.open_script = open;
            return Ok(Some(bounded));
        }
        // Only the three actual probe phases can interpret the retained original facts.
        _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
    }
    Ok(None)
}

impl MarkdownLineItemCoordinator {
    /// Consume only the original break job's actual grapheme demand after this
    /// caller's preceding quantum has ended.
    pub(in crate::markdown_line_composer) fn take_grapheme_demand(&mut self) -> Result<bool> {
        let result = (|| {
            self.guard()?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            Ok(payload
                .breaks
                .take_grapheme_source_demand(&self.authority)?)
        })();
        self.finish(result)
    }

    /// After grapheme demand, advance only the original grapheme dependency;
    /// preserve line-break/script/caller cursors without spending the same budget twice.
    pub(in crate::markdown_line_composer) fn advance_pending_graphemes(
        &mut self,
        budget: MarkdownLineComposeBudget,
    ) -> Result<MarkdownLineItemStep> {
        self.advance_pending_break_dependency(budget, true)
    }

    /// Advance the selected original break dependency under the existing
    /// frontier/paragraph authority and one actual source-work quantum.
    pub(super) fn advance_pending_break_dependency(
        &mut self,
        budget: MarkdownLineComposeBudget,
        grapheme: bool,
    ) -> Result<MarkdownLineItemStep> {
        let result = (|| {
            self.guard()?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            if !payload.frontier_enabled
                || (!payload.scripts_ready
                    && !payload
                        .bidi
                        .as_ref()
                        .is_some_and(MarkdownBidiParagraph::uses_prepared_paragraphs)
                    && !unicode_prefix_allowed(payload, &self.authority)?)
            {
                // Pending alone cannot grant prefix mode to a coordinator whose
                // original full-dependency or paragraph prerequisites are absent.
                return Err(MarkdownLineComposeError::InvalidRequest);
            }
            // Real bidi controls and trailing empty paragraphs may also demand
            // work; each query keeps its own Pending state and original Source ticket.
            let progress = payload.breaks.step_dependency(
                &self.authority,
                grapheme,
                MarkdownLineBreakBudget {
                    bytes: budget.bytes.min(4096),
                    scalars: budget.scalars,
                    records: budget.records,
                },
            )?;
            payload.breaks_ready = progress.ready;
            Ok(MarkdownLineItemStep {
                bytes: progress.bytes,
                context_bytes: 0,
                scalars: progress.scalars,
                records: progress.records,
                metadata_units: 0,
                shapes: 0,
                answer: MarkdownLineItemAnswer::Yielded,
            })
        })();
        self.finish(result)
    }

    /// Consume the current script demand through the same original script job;
    /// report its actual work without moving any item's source cursor.
    pub(super) fn advance_pending_scripts(
        &mut self,
        budget: MarkdownLineComposeBudget,
    ) -> Result<MarkdownLineItemStep> {
        let result = (|| {
            self.guard()?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            payload.script_source_demand = false;
            let progress = payload.scripts.step(
                &self.authority,
                MarkdownScriptBudget {
                    bytes: budget.bytes.min(4096),
                    scalars: budget.scalars,
                    records: budget.records,
                    stack_ops: budget.records,
                },
            )?;
            payload.scripts_ready = progress.status == crate::MarkdownScriptStatus::Ready;
            Ok(MarkdownLineItemStep {
                bytes: progress.bytes,
                context_bytes: 0,
                scalars: progress.scalars,
                records: progress.records,
                metadata_units: 0,
                shapes: 0,
                answer: MarkdownLineItemAnswer::Yielded,
            })
        })();
        self.finish(result)
    }
}
