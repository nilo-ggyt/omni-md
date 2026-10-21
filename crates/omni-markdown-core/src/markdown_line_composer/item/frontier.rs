//! Explicit frontier policy for the original coordinator. Only closed certificates
//! authorize glyph work; a short public range cannot create a shaping task.
use super::*;
use crate::markdown_layout_breaks::MarkdownCertifiedGraphemeEndpoint;
use crate::markdown_shaping::frontier::{
    MarkdownShapingFrontierAnswer as Answer, MarkdownShapingFrontierContinuation,
    MarkdownShapingFrontierError, MarkdownShapingFrontierJob, MarkdownShapingFrontierReason,
    MarkdownShapingTerminalGrapheme,
};
#[cfg(feature = "s2-table-window-prototype")]
mod style_boundary;
#[cfg(feature = "s2-table-window-prototype")]
pub(super) use style_boundary::{has_style_boundary_probe, provide_boundary_style};

/// Uncertified capabilities remain pending. These reasons do not mean missing
/// glyphs, a complete Ready paragraph, or permission to substitute default layout.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownLineFrontierReason {
    /// A narrow certificate rejected by the actual font plan. Keep the original
    /// type so callers can distinguish positioning, lookup behavior, and coverage.
    Plan(harfrust::AsciiFrontierRejection),
    /// The same fallback still covers following graphemes; bounded input was
    /// exhausted before a real font boundary was found.
    FontRunBeyondBudget,
    NonAscii,
    WordBeyondBudget,
    NotGraphemeBoundary,
    PendingConsumer,
    UncertifiedWord,
    NegativeAdvance,
    NonSimpleGrapheme,
}

/// Actual work performed by this frontier only. Original step facts independently
/// account for the Source-wide UBA, Script, and grapheme metadata phases.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MarkdownLineFrontierWork {
    pub source_bytes: usize,
    pub input_bytes: usize,
    pub context_bytes: usize,
    pub shape_calls: usize,
    pub parts: usize,
    pub metadata_glyphs: usize,
    pub plan_records: usize,
    pub boundary_records: usize,
}

/// One unresolved original item. Its continuation and real child's weak identity
/// do not grow with the number of parts already delivered.
pub(super) struct PendingFrontier {
    owner: MarkdownShapingFrontierJob,
    original: MarkdownLayoutFont,
    font: MarkdownLayoutFontAuthority,
    registration: Option<MarkdownLineFontRegistration>,
    replay_style: MarkdownLineComposeStyle,
    facts: MarkdownLineItemFacts,
    shaping_seams: [Option<ResolvedBidiShapingSeam>; 2],
    continuation: Option<MarkdownShapingFrontierContinuation>,
    child: Option<std::sync::Weak<()>>,
    complete: bool,
    open_script: bool,
    natural_tail: bool,
    terminal: Option<PendingTerminal>,
    declined_same_font: bool,
    declined_font_head: bool,
    // Keep only a weak registration for the platform base. Audit each completed
    // part's bounded grapheme window, never pre-scan the complete font run.
    preferred_registration: Option<MarkdownLineFontRegistration>,
    #[cfg(feature = "s2-table-window-prototype")]
    style_boundary: Option<style_boundary::StyleBoundary>,
}

/// A real blocking grapheme starts this fallback run. Store endpoint proofs, the
/// chosen font, and at most one following grapheme response, without retaining text.
struct PendingTerminal {
    proof: MarkdownShapingTerminalGrapheme,
    endpoints: [Option<MarkdownCertifiedGraphemeEndpoint>; 2],
    response: Option<MarkdownLineStyleResponse>,
    probe: Option<PendingFontProbe>,
    next: Option<SelectedFontGrapheme>,
    prefix_returned: bool,
    boundary_ready: bool,
    installed: bool,
    beyond_limit: bool,
}

/// One complete adjacent grapheme from the original Next query. Acquire its two
/// endpoint proofs over separately charged metadata quanta.
struct PendingFontProbe {
    range: Range<usize>,
    endpoints: [Option<MarkdownCertifiedGraphemeEndpoint>; 2],
}

/// An actual third font closes one run and starts the next. Consume this response
/// only after the preceding run's real child allocation has been released.
struct SelectedFontGrapheme {
    response: MarkdownLineStyleResponse,
    endpoints: [MarkdownCertifiedGraphemeEndpoint; 2],
}

impl PendingTerminal {
    fn new(proof: MarkdownShapingTerminalGrapheme) -> Self {
        let boundary_ready = proof.source_range().end == proof.item_end();
        Self {
            proof,
            endpoints: [None, None],
            response: None,
            probe: None,
            next: None,
            prefix_returned: false,
            boundary_ready,
            installed: false,
            beyond_limit: false,
        }
    }
}
impl PendingFrontier {
    /// Retain the original frontier only after child release. Complete facts matched
    /// owner.original at assembly and every part checks original_range; a center
    /// slice or published line cannot stand in for that support range. Semantic-edge
    /// and terminal-font responses may retain old publications, so restart them
    /// through normal preparation rather than prolonging their lifetimes here.
    #[cfg(feature = "s2-table-window-prototype")]
    pub(super) fn retains_selection_styles(&self, proven_end: usize) -> Result<bool> {
        if self.facts.source_utf16.end > proven_end
            || self.style_boundary.is_some()
            || self.terminal.is_some()
            || self
                .child
                .as_ref()
                .is_some_and(|child| child.strong_count() != 0)
        {
            return Ok(false);
        }
        if let Some(registration) = &self.registration
            && !self.original.permits(&registration.authority()?)
        {
            return Err(MarkdownLineComposeError::SourceMismatch);
        }
        if let Some(preferred) = &self.preferred_registration {
            preferred.lease()?;
        }
        Ok(true)
    }

    /// After a head-font trial finds no real right boundary, preserve the same
    /// rejected plan and original Script_Extensions/word-proof continuation.
    pub(in crate::markdown_line_composer::item) fn decline_font_head(&mut self) {
        self.declined_font_head = true;
    }

    /// Transfer the original frontier Job and its accepted response facts into one
    /// owner; no child, continuation, or terminal trial exists before actual work.
    pub(super) fn new(
        owner: MarkdownShapingFrontierJob,
        response: MarkdownLineStyleResponse,
        replay_style: MarkdownLineComposeStyle,
        facts: MarkdownLineItemFacts,
        open_script: bool,
        natural_tail: bool,
    ) -> Self {
        let font = response.font.authority();
        Self {
            owner,
            original: response.font,
            font,
            registration: response.registration,
            replay_style,
            facts,
            shaping_seams: response.request.shaping_seams.clone(),
            continuation: None,
            child: None,
            complete: false,
            open_script,
            natural_tail,
            terminal: None,
            declined_same_font: false,
            declined_font_head: false,
            preferred_registration: response.preferred_registration,
            #[cfg(feature = "s2-table-window-prototype")]
            style_boundary: response
                .root_leaf_boundary
                .map(style_boundary::StyleBoundary::new),
        }
    }
}

/// Charge each original endpoint independently, then issue one NeedStyle only when
/// both are certified. No replacement grapheme index or second Source is created.
fn prepare_terminal_style(
    payload: &mut Payload,
    authority: &MarkdownLayoutTextAuthority,
    budget: MarkdownLineComposeBudget,
    mut step: MarkdownLineItemStep,
) -> Result<MarkdownLineItemStep> {
    let pending = payload.frontier.as_mut().expect("原Frontier仍持暂停末GCB");
    let terminal = pending.terminal.as_mut().expect("原末GCB票仍在唯一槽内");
    let range = terminal.proof.source_range();
    for (index, offset) in [range.start, range.end].into_iter().enumerate() {
        // Keep previously acquired proofs across yields. Pending never supplies a
        // partial grapheme to a native font worker.
        if terminal.endpoints[index].is_some() {
            continue;
        }
        if step.metadata_units == budget.metadata_units {
            return Ok(step);
        }
        step.metadata_units += 1;
        payload.frontier_work.boundary_records = payload
            .frontier_work
            .boundary_records
            .checked_add(1)
            .ok_or(MarkdownLineComposeError::Overflow)?;
        let (answer, endpoint) = payload
            .breaks
            .certified_grapheme_endpoint(authority, offset)?;
        match answer {
            MarkdownGraphemeAnswer::Pending => return Ok(step),
            MarkdownGraphemeAnswer::Known(Some(actual)) if actual.utf16 == offset => {
                terminal.endpoints[index] =
                    Some(endpoint.ok_or(MarkdownLineComposeError::InconsistentCoverage)?);
            }
            _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
        }
    }
    let request = MarkdownLineStyleRequest {
        nonce: Arc::new(()),
        source: authority.clone(),
        range,
        scope: StyleScope::Text(TextStyleScope {
            paragraph: pending.facts.paragraph_utf16.clone(),
            script: pending.facts.script,
            level: pending.facts.level,
        }),
        shaping_seams: [None, None],
        continuation_font: Some(pending.font.clone()),
        continuation_bytes: Some(
            terminal.endpoints[0]
                .as_ref()
                .expect("原末 GCB 起点已认证")
                .boundary(authority)?
                .byte
                ..terminal.endpoints[1]
                    .as_ref()
                    .expect("原末 GCB 终点已认证")
                    .boundary(authority)?
                    .byte,
        ),
    };
    payload.request = Some(request.clone());
    payload.font_probe_scope = Some(Arc::new(()));
    payload.stage = Stage::Style;
    step.answer = MarkdownLineItemAnswer::NeedStyle(request);
    Ok(step)
}

/// A font response changes only the selected payload. Same-font success in the
/// middle does not close shaping context, so retain the original capability refusal.
pub(super) fn provide_terminal_style(
    payload: &mut Payload,
    authority: &MarkdownLayoutTextAuthority,
    response: MarkdownLineStyleResponse,
) -> Result<()> {
    let pending = payload.frontier.as_mut().expect("原Frontier持有字体暂停");
    let terminal = pending.terminal.as_mut().expect("原字体交接票仍在唯一槽内");
    let expected = terminal.probe.as_ref().map_or_else(
        || terminal.proof.source_range(),
        |probe| probe.range.clone(),
    );
    // A font trial cannot alter style and thereby invalidate the already certified
    // prefix's input. Validate the exact pending grapheme and live registration first.
    if response.coverage != expected
        || response.style.presentation != pending.replay_style.presentation
        || response.style.points.to_bits() != pending.replay_style.points.to_bits()
        || response.style.language != pending.replay_style.language
        || response.style.features != pending.replay_style.features
    {
        return Err(MarkdownLineComposeError::InvalidRequest);
    }
    if let Some(registration) = &response.registration {
        let registered = registration.lease()?;
        if !response.font.permits(&registered.font().authority()) {
            return Err(MarkdownLineComposeError::SourceMismatch);
        }
    }
    let original_selected = response.font.permits(&pending.font);
    if let Some(probe) = terminal.probe.take() {
        if original_selected {
            terminal.boundary_ready = true;
            // Actual original-font coverage closes the fallback run here. This
            // following original-font grapheme has not yet been consumed.
        } else if response.font.permits(
            &terminal
                .response
                .as_ref()
                .expect("回退段持有已选Font")
                .font
                .authority(),
        ) {
            let [_, end] = probe.endpoints;
            terminal.endpoints[1] = end;
            let first = terminal.endpoints[0]
                .as_ref()
                .expect("段首原端点")
                .boundary(authority)?;
            let last = terminal.endpoints[1]
                .as_ref()
                .expect("试次前已取得段末票")
                .boundary(authority)?;
            terminal.beyond_limit = last.utf16 - first.utf16 > closed_item::MAX_CLOSED_UTF16
                || last.byte - first.byte > closed_item::MAX_CLOSED_BYTES;
            terminal.boundary_ready = last.utf16 == pending.facts.source_utf16.end;
            // Coalesce the same fallback payload. A fixed allowance cannot close
            // the real run; an oversized continuous run remains explicitly unsupported.
        } else {
            terminal.next = Some(SelectedFontGrapheme {
                response,
                endpoints: probe
                    .endpoints
                    .map(|value| value.expect("完整相邻GCB原端点")),
            });
            terminal.boundary_ready = true;
            // A real third-font choice supplies the right edge. Retain only this
            // already prepared successor trial until the current child retires.
        }
    } else if original_selected && expected.end != pending.facts.source_utf16.end {
        pending.terminal = None;
        pending.declined_same_font = true;
        // Same-font coverage in the middle does not certify the local shaping plan.
        // Do not invent a boundary or endlessly reissue the same trial.
    } else {
        pending
            .owner
            .resume_terminal_grapheme(authority, &pending.font, &terminal.proof, original_selected)
            .map_err(map_error)?;
        if original_selected {
            pending.terminal = None; // Finish the natural tail through its original plan.
        } else {
            terminal.response = Some(response); // Deliver the old-font remainder first.
        }
    }
    payload.request.take();
    payload.font_probe_scope.take(); // Accepted responses expire the original trial scope.
    payload.stage = Stage::Frontier;
    Ok(())
}

/// Trial adjacent graphemes only inside the fallback region. Actual original-font
/// coverage resumes the original batched layout path.
fn advance_font_run(
    payload: &mut Payload,
    authority: &MarkdownLayoutTextAuthority,
    budget: MarkdownLineComposeBudget,
    mut step: MarkdownLineItemStep,
) -> Result<MarkdownLineItemStep> {
    let pending = payload.frontier.as_mut().expect("唯一Font交接owner");
    let terminal = pending.terminal.as_mut().expect("真实Font段仍待消费");
    if terminal.beyond_limit {
        step.answer =
            MarkdownLineItemAnswer::NeedsFrontier(MarkdownLineFrontierReason::FontRunBeyondBudget);
        return Ok(step); // An input allowance is not a font edge; never truncate the run.
    }
    if terminal.boundary_ready {
        install_terminal_font(payload, authority)?;
        return Ok(step);
    }
    if terminal.probe.is_none() {
        // Ask the original grapheme index for one adjacent complete range, preserving
        // the original item's end rather than guessing from Source length.
        let start = terminal.endpoints[1]
            .as_ref()
            .expect("已选段末端点")
            .boundary(authority)?
            .utf16;
        step.metadata_units += 1;
        payload.frontier_work.boundary_records = payload
            .frontier_work
            .boundary_records
            .checked_add(1)
            .ok_or(MarkdownLineComposeError::Overflow)?;
        let next = payload
            .breaks
            .grapheme_query(authority, start, MarkdownGraphemeQuery::Next)?;
        let end = match next {
            MarkdownGraphemeAnswer::Pending => return Ok(step),
            MarkdownGraphemeAnswer::Known(Some(boundary))
                if boundary.utf16 > start && boundary.utf16 <= pending.facts.source_utf16.end =>
            {
                boundary.utf16
            }
            _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
        };
        terminal.probe = Some(PendingFontProbe {
            range: start..end,
            endpoints: [None, None],
        });
    }
    let probe = terminal.probe.as_mut().expect("原Next查询已确定相邻GCB");
    for (index, offset) in [probe.range.start, probe.range.end].into_iter().enumerate() {
        if probe.endpoints[index].is_some() {
            continue;
        }
        if step.metadata_units == budget.metadata_units {
            return Ok(step);
        }
        step.metadata_units += 1;
        payload.frontier_work.boundary_records = payload
            .frontier_work
            .boundary_records
            .checked_add(1)
            .ok_or(MarkdownLineComposeError::Overflow)?;
        let (answer, endpoint) = payload
            .breaks
            .certified_grapheme_endpoint(authority, offset)?;
        match answer {
            MarkdownGraphemeAnswer::Pending => return Ok(step),
            MarkdownGraphemeAnswer::Known(Some(actual)) if actual.utf16 == offset => {
                probe.endpoints[index] =
                    Some(endpoint.ok_or(MarkdownLineComposeError::InconsistentCoverage)?);
            }
            _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
        }
    } // Both original proofs are required; no text read or guessed line height occurs.
    let request = MarkdownLineStyleRequest {
        nonce: Arc::new(()),
        source: authority.clone(),
        range: probe.range.clone(),
        scope: StyleScope::Text(TextStyleScope {
            paragraph: pending.facts.paragraph_utf16.clone(),
            script: pending.facts.script,
            level: pending.facts.level,
        }),
        shaping_seams: [None, None],
        continuation_font: Some(pending.font.clone()),
        continuation_bytes: Some(
            probe.endpoints[0]
                .as_ref()
                .expect("原相邻 GCB 起点已认证")
                .boundary(authority)?
                .byte
                ..probe.endpoints[1]
                    .as_ref()
                    .expect("原相邻 GCB 终点已认证")
                    .boundary(authority)?
                    .byte,
        ),
    };
    payload.request = Some(request.clone());
    payload.font_probe_scope = Some(Arc::new(()));
    payload.stage = Stage::Style;
    step.answer = MarkdownLineItemAnswer::NeedStyle(request);
    Ok(step)
}

/// Assemble a complete font run only between real font boundaries/natural item
/// ends. The original frontier remains alive until its child is actually released.
fn install_terminal_font(
    payload: &mut Payload,
    authority: &MarkdownLayoutTextAuthority,
) -> Result<()> {
    payload.source.health()?;
    let pending = payload.frontier.as_mut().expect("原Font残余owner仍活");
    let terminal = pending.terminal.as_mut().expect("真实Font边界票仍在");
    let response = terminal.response.take().expect("已接受的新Font响应");
    let endpoints = [
        terminal.endpoints[0].take().expect("原段首票"),
        terminal.endpoints[1].take().expect("原段末票"),
    ];
    let first = endpoints[0].boundary(authority)?;
    let last = endpoints[1].boundary(authority)?;
    let range = first.utf16..last.utf16;
    // The already delivered prefix remains unchanged. This fallback run cannot
    // escape the original Script_Extensions, style, or bidi item coverage.
    if payload.cursor != range.start
        || !terminal.boundary_ready
        || range.end > pending.facts.source_utf16.end
    {
        return Err(MarkdownLineComposeError::InconsistentCoverage);
    }
    let font = response.font.authority();
    let closed = closed_item::ClosedItem::from_terminal_font_change(
        &terminal.proof,
        endpoints,
        authority,
        &font,
    )?;
    let mut facts = pending.facts.clone();
    facts.source_utf16 = range.clone();
    facts.beginning_of_text = range.start == facts.paragraph_utf16.start;
    facts.end_of_text = range.end == facts.paragraph_utf16.end;
    let style = response.style;
    let options = MarkdownShapingOptions {
        direction: facts.direction,
        script: facts.script,
        language: style.language.clone(),
        points: facts.points,
        beginning_of_text: facts.beginning_of_text,
        end_of_text: facts.end_of_text,
        features: style.features.clone(),
    };
    // Transfer the same Source and selected payload to the original pending input
    // preparation; this does not advance the frontier's private cursor yet.
    let shaping = crate::markdown_shaping::PendingClosedShaping::new(
        payload.source.clone(),
        authority,
        response.font,
        &font,
        closed,
        options,
    )?;
    terminal.installed = true;
    payload.closed_input = Some(closed_item::PendingItem {
        shaping,
        font,
        registration: response.registration,
        replay_style: style,
        facts,
        shaping_seams: [None, None],
    });
    payload.stage = Stage::ClosedInput;
    Ok(()) // Only actual run-child release later permits the original plan to advance.
}

/// After take_item checks actual Ready/EOF, register this same new-font child. A
/// coordinate receipt cannot substitute for release of the real Job allocation.
pub(super) fn lend_font_item(
    payload: &mut Payload,
    authority: &MarkdownLayoutTextAuthority,
    item: &mut MarkdownLineItem,
) -> Result<()> {
    let Some(pending) = payload.frontier.as_mut() else {
        return Ok(());
    };
    let Some(terminal) = pending
        .terminal
        .as_ref()
        .filter(|terminal| terminal.installed)
    else {
        return Ok(());
    };
    let continuation = pending
        .owner
        .lend_font_run(authority, &pending.font, &terminal.proof, &mut item.job)
        .map_err(map_error)?;
    pending.child = Some(item.job.index_identity());
    pending.continuation = Some(continuation);
    pending.complete = item.facts.source_utf16.end == pending.facts.source_utf16.end;
    Ok(())
}

/// After actual child release, the already prepared third-font trial may become
/// the next run. Without such a successor, the original font resumes batched work.
fn finish_font_run(payload: &mut Payload, authority: &MarkdownLayoutTextAuthority) -> Result<()> {
    let pending = payload.frontier.as_mut().expect("归还Font段仍属于原owner");
    let mut terminal = pending.terminal.take().expect("原Font交接唯一槽");
    if let Some(next) = terminal.next.take() {
        let proof = pending
            .owner
            .following_font_grapheme(
                authority,
                &pending.font,
                &terminal.proof,
                [&next.endpoints[0], &next.endpoints[1]],
            )
            .map_err(map_error)?;
        let mut following = PendingTerminal::new(proof);
        following.endpoints = next.endpoints.map(Some);
        following.response = Some(next.response);
        following.prefix_returned = true;
        pending.terminal = Some(following);
    } // No successor: retain original font/plan/Script/Source on their existing owner.
    Ok(())
}

impl MarkdownLineItemCoordinator {
    /// Explicitly enable certified frontier work. The original constructor retains
    /// complete-item behavior and cannot itself establish lazy-layout acceptance.
    pub fn new_with_frontier(
        source: MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        cancellation: MarkdownLineComposeCancellation,
    ) -> Result<Self> {
        let mut result = Self::new(source, authority, cancellation)?;
        result.enable_frontier()?;
        Ok(result)
    }
    pub(in crate::markdown_line_composer) fn enable_frontier(&mut self) -> Result<()> {
        self.guard()?;
        let payload = self
            .payload
            .as_mut()
            .ok_or(MarkdownLineComposeError::Poisoned)?;
        if !matches!(payload.stage, Stage::Bidi) || payload.cursor != 0 {
            return Err(MarkdownLineComposeError::InvalidRequest);
        }
        // Select before the first step; an in-flight complete Job cannot be
        // retroactively certified as frontier work.
        payload.frontier_enabled = true;
        Ok(())
    }
    pub fn frontier_work(&self) -> Result<MarkdownLineFrontierWork> {
        self.guard()?;
        Ok(self
            .payload
            .as_ref()
            .ok_or(MarkdownLineComposeError::Poisoned)?
            .frontier_work)
    }
}

pub(super) fn map_error(error: MarkdownShapingFrontierError) -> MarkdownLineComposeError {
    // Preserve typed failures at their existing owning boundary. A continuation
    // mismatch is authority failure; it must not turn into a retryable pending reason.
    match error {
        MarkdownShapingFrontierError::Shaping(error) => MarkdownLineComposeError::Shaping(error),
        MarkdownShapingFrontierError::Breaks(error) => MarkdownLineComposeError::Break(error),
        MarkdownShapingFrontierError::ContinuationMismatch => {
            MarkdownLineComposeError::SourceMismatch
        }
        MarkdownShapingFrontierError::Poisoned => MarkdownLineComposeError::Poisoned,
        MarkdownShapingFrontierError::Advance(error) => {
            MarkdownLineComposeError::FrontierMetadata(Box::new(error))
        }
    }
}
fn reason(value: MarkdownShapingFrontierReason) -> MarkdownLineFrontierReason {
    // Keep capability refusal distinct from fatal errors and from glyph coverage.
    match value {
        MarkdownShapingFrontierReason::Plan(reason) => MarkdownLineFrontierReason::Plan(reason),
        MarkdownShapingFrontierReason::NonAscii => MarkdownLineFrontierReason::NonAscii,
        MarkdownShapingFrontierReason::WordBeyondBudget => {
            MarkdownLineFrontierReason::WordBeyondBudget
        }
        MarkdownShapingFrontierReason::NotGraphemeBoundary => {
            MarkdownLineFrontierReason::NotGraphemeBoundary
        }
        MarkdownShapingFrontierReason::UncertifiedWord => {
            MarkdownLineFrontierReason::UncertifiedWord
        }
        MarkdownShapingFrontierReason::NegativeAdvance => {
            MarkdownLineFrontierReason::NegativeAdvance
        }
        MarkdownShapingFrontierReason::NonSimpleGrapheme => {
            MarkdownLineFrontierReason::NonSimpleGrapheme
        }
    }
}

pub(super) fn advance(
    payload: &mut Payload,
    authority: &MarkdownLayoutTextAuthority,
    cancellation: &MarkdownLineComposeCancellation,
    budget: MarkdownLineComposeBudget,
) -> Result<MarkdownLineItemStep> {
    let mut step = MarkdownLineItemStep {
        bytes: 0,
        context_bytes: 0,
        scalars: 0,
        records: 0,
        metadata_units: 0,
        shapes: 0,
        answer: MarkdownLineItemAnswer::Yielded,
    };
    #[cfg(feature = "s2-table-window-prototype")]
    if has_style_boundary_probe(payload) {
        return style_boundary::prepare(payload, authority, budget, step);
    }
    if payload.frontier.as_ref().is_some_and(|pending| {
        pending
            .terminal
            .as_ref()
            .is_some_and(|tail| tail.response.is_none() && !tail.installed)
    }) {
        return prepare_terminal_style(payload, authority, budget, step);
    } // Endpoint work uses a later quantum, not an already exhausted Source budget.
    {
        let pending = payload
            .frontier
            .as_mut()
            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
        if pending.continuation.is_some() {
            if pending
                .child
                .as_ref()
                .is_none_or(|child| child.strong_count() != 0)
            {
                step.answer = MarkdownLineItemAnswer::NeedsFrontier(
                    MarkdownLineFrontierReason::PendingConsumer,
                );
                // Resume requires actual child release; calling it early would
                // poison the owner. Do not allocate a replacement Job while waiting.
                return Ok(step);
            }
            pending
                .owner
                .resume(
                    authority,
                    &pending.font,
                    pending
                        .continuation
                        .take()
                        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?,
                )
                .map_err(map_error)?;
            pending.child = None;
            if pending.terminal.as_ref().is_some_and(|run| run.installed) {
                finish_font_run(payload, authority)?;
            } else if let Some(run) = &mut pending.terminal {
                run.prefix_returned = true;
            } // Both old-font remainder and fallback run wait for actual Store release.
            let pending = payload.frontier.as_mut().expect("原Frontier不在归还中丢失");
            if pending.complete {
                if (!pending.open_script && payload.cursor != pending.facts.source_utf16.end)
                    || (pending.open_script && payload.cursor >= pending.facts.source_utf16.end)
                {
                    return Err(MarkdownLineComposeError::InconsistentCoverage);
                }
                drop(payload.frontier.take());
                payload.stage = Stage::ItemStart;
                // A closed item reaches its real end; an open scope reaches only
                // its center end. Both transitions occur after actual child release.
                return Ok(step);
            }
        }
    }
    let pending = payload
        .frontier
        .as_mut()
        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
    if pending
        .terminal
        .as_ref()
        .is_some_and(|run| run.prefix_returned && !run.installed)
    {
        return advance_font_run(payload, authority, budget, step);
    }
    let progress = pending
        .owner
        .step(
            authority,
            &pending.font,
            &mut payload.breaks,
            MarkdownShapingBudget {
                bytes: budget.bytes.min(4096),
                scalars: budget.scalars,
            },
            budget.metadata_units,
        )
        .map_err(map_error)?;
    step.bytes = progress
        .source_bytes
        .checked_add(progress.input_bytes)
        .ok_or(MarkdownLineComposeError::Overflow)?;
    step.scalars = progress.scalars; // Charge actual decoded scalars, not UTF-8 byte count.
    step.shapes = progress.shapes;
    step.context_bytes = progress.context_bytes;
    step.metadata_units = progress
        .metadata_glyphs
        .checked_add(progress.plan_records)
        .and_then(|units| units.checked_add(progress.boundary_records))
        .filter(|units| *units <= budget.metadata_units)
        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
    // Accumulate the original owner's actual work without resetting it at each
    // part or font trial. Overflow and exceeded metadata remain explicit failures.
    let work = &mut payload.frontier_work;
    work.source_bytes = work
        .source_bytes
        .checked_add(progress.source_bytes)
        .ok_or(MarkdownLineComposeError::Overflow)?;
    work.input_bytes = work
        .input_bytes
        .checked_add(progress.input_bytes)
        .ok_or(MarkdownLineComposeError::Overflow)?;
    work.context_bytes = work
        .context_bytes
        .checked_add(progress.context_bytes)
        .ok_or(MarkdownLineComposeError::Overflow)?;
    work.shape_calls = work
        .shape_calls
        .checked_add(progress.shapes)
        .ok_or(MarkdownLineComposeError::Overflow)?;
    work.metadata_glyphs = work
        .metadata_glyphs
        .checked_add(progress.metadata_glyphs)
        .ok_or(MarkdownLineComposeError::Overflow)?;
    work.plan_records = work
        .plan_records
        .checked_add(progress.plan_records)
        .ok_or(MarkdownLineComposeError::Overflow)?;
    work.boundary_records = work
        .boundary_records
        .checked_add(progress.boundary_records)
        .ok_or(MarkdownLineComposeError::Overflow)?;
    match progress.answer {
        Answer::Pending => {}
        Answer::NeedsFrontier(value) => {
            // A head-font trial is allowed only before consuming this original
            // item, and only for refusals that could hide a real font transition.
            let at_unconsumed_head = payload.cursor == pending.facts.source_utf16.start
                && pending.child.is_none()
                && pending.terminal.is_none()
                && !pending.declined_font_head
                && (pending.facts.paragraph_utf16.end - payload.cursor
                    > closed_item::MAX_CLOSED_UTF16
                    || (!pending.open_script && !pending.natural_tail));
            if at_unconsumed_head
                && matches!(
                    value,
                    MarkdownShapingFrontierReason::Plan(
                        harfrust::AsciiFrontierRejection::Positioning
                            | harfrust::AsciiFrontierRejection::CrossInputSubtable
                            | harfrust::AsciiFrontierRejection::LookupBehavior
                    ) | MarkdownShapingFrontierReason::NonAscii
                )
            {
                let ascii_suffix = if pending.open_script {
                    let span = payload
                        .bidi
                        .as_ref()
                        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?
                        .uniform_control_free_span(
                            authority,
                            payload.cursor..payload.cursor.saturating_add(1),
                        )?;
                    if let Some(span) = span {
                        payload.scripts.prepare_ascii_suffix(
                            authority,
                            payload.cursor,
                            span.content_end(),
                            span.paragraph_range().end,
                        )?
                    } else {
                        false
                    }
                } else {
                    false
                }; // Without an original same-Source bidi span, do not guess separators.
                if ascii_suffix {
                    match payload.scripts.run_at(
                        authority,
                        MarkdownScriptUnit::Utf16,
                        payload.cursor,
                    )? {
                        MarkdownScriptAnswer::Pending => payload.script_source_demand = true,
                        MarkdownScriptAnswer::Known(Some(_)) => {
                            // Retire the open plan only after the original Script
                            // index has a real closed run. Its request chain obtains
                            // the same font/style through normal assembly again.
                            payload.closed_script_at = Some(payload.cursor);
                            drop(payload.frontier.take());
                            payload.open_script = false;
                            payload.stage = Stage::Dependencies;
                        }
                        _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
                    }
                    return Ok(step); // Original Script work consumes its next own quantum.
                }
                // Non-ASCII suffixes, unsettled context, or absent same-Source ends
                // stay with original FontRuns. The first grapheme's font never
                // proves coverage for a huge Script_Extensions item: an initial
                // Mono space, for example, says nothing about following Chinese.
                // Positioning/cross-input refusals grant no arbitrary split either.
                // Only an adjacent complete grapheme selecting another real payload
                // closes this head; same-font marks/ligatures remain intact. If no
                // change is found, retire this probe and keep the precise refusal
                // and original Script/word proof. The original owner retains Source,
                // font, cancellation, and resource scope until its real child ends.
                // This also covers an unconsumed short semantic run whose end is
                // not natural; short style coverage alone never grants closure.
                payload.closed_fonts = Some(closed_item::FontRuns::frontier_head(
                    authority.clone(),
                    pending.original.clone(),
                    pending.registration.clone(),
                    pending.replay_style.clone(),
                    pending.facts.clone(),
                    cancellation.shaping(),
                    pending.shaping_seams.clone(),
                ));
                payload.stage = Stage::ClosedFonts;
                return Ok(step);
            }
            #[cfg(feature = "s2-table-window-prototype")]
            if style_boundary::begin(payload)? {
                // The head already spent this step's plan budget. Charge the right
                // grapheme on the next step, without spending the metadata quota twice.
                return Ok(step);
            }
            let pending = payload.frontier.as_mut().expect("原失败计划仍在同owner内");
            if pending.open_script {
                // An open Script_Extensions run cannot switch to a finite-context
                // ASCII plan. Continue original dependency preparation until actual
                // closure, then request the complete intersection's style. Release
                // the old response/resources without extending its font authority.
                payload.closed_script_at = Some(payload.cursor);
                drop(payload.frontier.take());
                payload.open_script = false;
                payload.stage = Stage::Dependencies;
            } else {
                if pending.natural_tail
                    && !pending.declined_same_font
                    && let Some(proof) = pending
                        .owner
                        .terminal_grapheme(authority, &pending.font)
                        .map_err(map_error)?
                {
                    pending.terminal = Some(PendingTerminal::new(proof));
                } else {
                    step.answer = MarkdownLineItemAnswer::NeedsFrontier(reason(value));
                } // Retry only the blocking grapheme inside the original complete run.
            }
        } // Uncertified plans stay pending; no complete-item fallback hides glyph work.
        Answer::PartReady => {
            let part = pending
                .owner
                .take_part(authority, &pending.font)
                .map_err(map_error)?;
            let range = part.certified_range();
            let item_complete = part.is_item_complete();
            let complete = part.is_scope_complete();
            // Validate the actual original range and current cursor before moving
            // the child. A safe open-scope end is not a complete natural item end.
            if part.original_range() != pending.facts.source_utf16
                || range.start != payload.cursor
                || range.is_empty()
                || range.end > pending.facts.source_utf16.end
                || item_complete != (range.end == pending.facts.source_utf16.end)
                || (pending.open_script && item_complete)
            {
                return Err(MarkdownLineComposeError::InconsistentCoverage);
            }
            let audit = closed_item::FontRuns::for_frontier_part(
                &part,
                pending.original.clone(),
                pending.registration.clone(),
                pending.replay_style.clone(),
                pending.facts.clone(),
                pending.preferred_registration.clone(),
            )?;
            let (mut job, continuation) = part.into_parts();
            let profile = job.profile()?;
            job.glyph(authority, &pending.font, profile.glyph_count)?;
            let mut facts = pending.facts.clone();
            facts.source_utf16 = range;
            facts.beginning_of_text = facts.source_utf16.start == facts.paragraph_utf16.start;
            facts.end_of_text = facts.source_utf16.end == facts.paragraph_utf16.end;
            pending.child = Some(job.index_identity());
            pending.continuation = Some(continuation);
            pending.complete = complete;
            let mut shaping_seams = [None, None];
            for (index, seam) in pending.shaping_seams.iter().enumerate() {
                let Some(seam) = seam else { continue };
                let (_, at) = seam.boundary(authority)?;
                let endpoint = if index == 0 {
                    facts.source_utf16.start
                } else {
                    facts.source_utf16.end
                };
                if at == endpoint {
                    shaping_seams[index] = Some(seam.clone());
                }
            } // A stale Source returns its typed error, never a silently missing seam.
            payload.item = Some(MarkdownLineItem {
                job: crate::markdown_line_composer::worker_resources::ShapeResourceOwner::new(
                    job,
                    &payload.resources,
                ),
                source: payload.source.clone(),
                authority: authority.clone(),
                font: pending.font.clone(),
                registration: pending.registration.clone(),
                replay_style: pending.replay_style.clone(),
                facts: facts.clone(),
                shaping_seams,
            });
            work.parts = work
                .parts
                .checked_add(1)
                .ok_or(MarkdownLineComposeError::Overflow)?;
            payload.stage = Stage::Ready;
            step.answer = MarkdownLineItemAnswer::ItemReady(facts);
            if let Some(audit) = audit {
                // Audit this same sealed Job without another glyph graph. Unchanged
                // fonts return its original continuation intact. After a real font
                // change, retire the parent only after the reshaped child releases;
                // never resume it at the stale whole-part endpoint.
                payload.closed_fonts = Some(audit);
                payload.stage = Stage::ClosedFonts;
                step.answer = MarkdownLineItemAnswer::Yielded;
            }
        } // Store must release the real Ready child before returning its continuation.
        Answer::PendingConsumer | Answer::Complete => {
            return Err(MarkdownLineComposeError::InconsistentCoverage);
        } // These transitions require the original ticket handoff and actual child release.
    }
    Ok(step)
}
