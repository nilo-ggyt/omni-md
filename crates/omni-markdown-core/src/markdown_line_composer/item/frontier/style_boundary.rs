//! Adjacent original RootLeaf semantics authorize one complete right-grapheme trial.
//! Only a real font change closes the left item; right-side text stays unconsumed.

use super::*;
use crate::VerifiedMarkdownRootLeafStyleBoundary;
use crate::markdown_document_buffer::MarkdownTableRequestError;

pub(super) struct StyleBoundary {
    // Retain the verified publication ticket, not caller-supplied adjacent ranges.
    // A completed or declined trial is not retried under the same boundary owner.
    ticket: VerifiedMarkdownRootLeafStyleBoundary,
    probe: Option<PendingFontProbe>,
    declined: bool,
}

impl StyleBoundary {
    pub(super) fn new(ticket: VerifiedMarkdownRootLeafStyleBoundary) -> Self {
        Self {
            ticket,
            probe: None,
            declined: false,
        }
    }
}

fn source_error(error: MarkdownTableRequestError) -> MarkdownLineComposeError {
    // Keep original Source failure typed; other publication-proof failures retain
    // their own cause instead of becoming a font capability refusal.
    match error {
        MarkdownTableRequestError::Source(error) => MarkdownLineComposeError::Source(error),
        other => MarkdownLineComposeError::RootLeafBoundary(Box::new(other)),
    }
}

/// Inspect the adjacent right semantic run only after the original per-grapheme
/// font audit reached the complete short left item's end. A huge item cannot enter
/// this path or read its complete body merely to reach a real style boundary.
pub(super) fn begin(payload: &mut Payload) -> Result<bool> {
    let pending = payload.frontier.as_mut().expect("原前沿仍持唯一样式owner");
    let Some(boundary) = &mut pending.style_boundary else {
        return Ok(false);
    };
    // Admission needs the original unconsumed, closed short item and no retained
    // child/terminal trial. Do not combine independent font-transfer lifecycles.
    if boundary.declined
        || boundary.probe.is_some()
        || !pending.declined_font_head
        || pending.open_script
        || pending.child.is_some()
        || pending.terminal.is_some()
        || pending.facts.source_utf16.start != payload.cursor
        || pending.facts.source_utf16.len() > closed_item::MAX_CLOSED_UTF16
    {
        return Ok(false);
    }
    // Revalidate the actual publication against the same Source and accepted left
    // style before deriving the right probe's start from that certified endpoint.
    boundary
        .ticket
        .require_source(&payload.source)
        .map_err(source_error)?;
    if pending.facts.source_utf16.end != boundary.ticket.left().utf16_range.end
        || pending.replay_style.presentation != boundary.ticket.left().styles
    {
        return Err(MarkdownLineComposeError::SourceMismatch);
    }
    let end = pending.facts.source_utf16.end;
    boundary.probe = Some(PendingFontProbe {
        range: end..end,
        endpoints: [None, None],
    });
    Ok(true)
}

pub(in crate::markdown_line_composer::item) fn has_style_boundary_probe(payload: &Payload) -> bool {
    payload
        .frontier
        .as_ref()
        .and_then(|pending| pending.style_boundary.as_ref())
        .is_some_and(|boundary| boundary.probe.is_some())
}

/// Charge the original Next query and two endpoint proofs separately. Native font
/// selection cannot receive half a surrogate pair or combining sequence.
pub(super) fn prepare(
    payload: &mut Payload,
    authority: &MarkdownLayoutTextAuthority,
    budget: MarkdownLineComposeBudget,
    step: MarkdownLineItemStep,
) -> Result<MarkdownLineItemStep> {
    // Attribute only work actually performed by this continuation to the frontier.
    let before = step.metadata_units;
    let result = prepare_gcb(payload, authority, budget, step)?;
    payload.frontier_work.boundary_records = payload
        .frontier_work
        .boundary_records
        .checked_add(result.metadata_units - before)
        .ok_or(MarkdownLineComposeError::Overflow)?;
    Ok(result)
}

fn prepare_gcb(
    payload: &mut Payload,
    authority: &MarkdownLayoutTextAuthority,
    budget: MarkdownLineComposeBudget,
    mut step: MarkdownLineItemStep,
) -> Result<MarkdownLineItemStep> {
    let pending = payload.frontier.as_mut().expect("相邻试次仍属于原前沿");
    let boundary = pending.style_boundary.as_mut().expect("真实RootLeaf票仍活");
    boundary
        .ticket
        .require_source(&payload.source)
        .map_err(source_error)?;
    let probe = boundary.probe.as_mut().expect("只有已开启右GCB才推进");
    if probe.range.is_empty() {
        // Establish one complete right grapheme inside both the verified semantic
        // range and its original request, yielding on unresolved original metadata.
        if step.metadata_units == budget.metadata_units {
            return Ok(step);
        }
        step.metadata_units += 1;
        match payload.breaks.grapheme_query(
            authority,
            probe.range.start,
            MarkdownGraphemeQuery::Next,
        )? {
            MarkdownGraphemeAnswer::Pending => return Ok(step),
            MarkdownGraphemeAnswer::Known(Some(end))
                if end.utf16 > probe.range.start
                    && end.utf16 <= boundary.ticket.right().utf16_range.end
                    && end.utf16 <= boundary.ticket.request().range.end =>
            {
                probe.range.end = end.utf16
            }
            _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
        }
    }
    for (index, offset) in [probe.range.start, probe.range.end].into_iter().enumerate() {
        // Reuse certified endpoints across quanta. Pending does not issue a partial
        // request, and a non-grapheme semantic edge does not manufacture a proof.
        if probe.endpoints[index].is_some() {
            continue;
        }
        if step.metadata_units == budget.metadata_units {
            return Ok(step);
        }
        step.metadata_units += 1;
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
    }
    // Bind exact original byte coordinates and paragraph facts to a fresh nonce.
    // The right glyphs are not consumed here; they only witness a possible font edge.
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
                .expect("右GCB首票已取得")
                .boundary(authority)?
                .byte
                ..probe.endpoints[1]
                    .as_ref()
                    .expect("右GCB尾票已取得")
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

/// Accept a complete grapheme under the actual right style. A changed semantic
/// style with the same payload returns to the original plan refusal. The head trial
/// already proved the left item's font; the right payload only witnesses an edge,
/// without retaining another glyph graph.
pub(in crate::markdown_line_composer::item) fn provide_boundary_style(
    payload: &mut Payload,
    authority: &MarkdownLayoutTextAuthority,
    cancellation: &MarkdownLineComposeCancellation,
    response: MarkdownLineStyleResponse,
) -> Result<()> {
    let pending = payload.frontier.as_mut().expect("原左Font与计划仍活");
    let boundary = pending.style_boundary.as_mut().expect("同Source相邻票仍活");
    boundary
        .ticket
        .require_source(&payload.source)
        .map_err(source_error)?;
    let probe = boundary.probe.as_ref().expect("原右NeedStyle持完整GCB");
    // The response must belong to this exact right-side trial/publication. If it
    // names a registration, lease that actual payload before consuming the trial.
    if response.coverage != probe.range
        || response.style.presentation != boundary.ticket.right().styles
    {
        return Err(MarkdownLineComposeError::InvalidRequest);
    }
    if let Some(registration) = &response.registration {
        let registered = registration.lease()?;
        if !response.font.permits(&registered.font().authority()) {
            return Err(MarkdownLineComposeError::SourceMismatch);
        }
    }
    let changed = !response.font.permits(&pending.font);
    boundary.probe = None;
    boundary.declined = true;
    payload.request = None;
    payload.font_probe_scope = None;
    if changed {
        // The real payload transition closes only the original left item. Its
        // existing source/style/cancellation chain owns the pending reshape.
        payload.closed_fonts = Some(closed_item::FontRuns::style_boundary(
            authority.clone(),
            pending.original.clone(),
            pending.registration.clone(),
            pending.replay_style.clone(),
            pending.facts.clone(),
            cancellation.shaping(),
            pending.shaping_seams.clone(),
        ));
        payload.stage = Stage::ClosedFonts;
    } else {
        // Link color or another semantic difference does not prove shaping context
        // can be cut here when the actual selected font payload remains unchanged.
        payload.stage = Stage::Frontier;
    }
    Ok(())
}

impl MarkdownLineItemCoordinator {
    /// Only the current right-side NeedStyle may borrow this original ticket. A
    /// stale nonce at identical coordinates cannot renew publication authority.
    pub(in crate::markdown_line_composer) fn root_leaf_style_boundary_for(
        &self,
        request: &MarkdownLineStyleRequest,
    ) -> Result<Option<&VerifiedMarkdownRootLeafStyleBoundary>> {
        self.guard()?;
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownLineComposeError::Poisoned)?;
        let current = payload
            .request
            .as_ref()
            .ok_or(MarkdownLineComposeError::InvalidRequest)?;
        // Check the one current request before exposing the borrowed publication;
        // then recheck its original Source rather than recapturing current state.
        if !matches!(payload.stage, Stage::Style)
            || !Arc::ptr_eq(&current.nonce, &request.nonce)
            || request.source != self.authority
        {
            return Err(MarkdownLineComposeError::SourceMismatch);
        }
        let Some(boundary) = payload
            .frontier
            .as_ref()
            .and_then(|pending| pending.style_boundary.as_ref())
            .filter(|boundary| boundary.probe.is_some())
        else {
            return Ok(None);
        };
        boundary
            .ticket
            .require_source(&payload.source)
            .map_err(source_error)?;
        Ok(Some(&boundary.ticket))
    }
}
