//! Prepare only the actual bidi paragraph at this original item position.
//! Dependency and endpoint work share one quantum without copied text or directories.

use super::*;

/// Resume the original paragraph preparation and return readiness separately
/// from the source/scalar/metadata work already consumed during this step.
pub(super) fn prepare(
    payload: &mut Payload,
    authority: &MarkdownLayoutTextAuthority,
    budget: MarkdownLineComposeBudget,
    step: &mut MarkdownLineItemStep,
) -> Result<bool> {
    let bidi = payload
        .bidi
        .as_mut()
        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
    if !bidi.uses_prepared_paragraphs() {
        // Existing single-paragraph/paged representations need no endpoint
        // preparation; outer Source/cancellation guards remain authoritative.
        return Ok(true);
    }
    let budget = MarkdownBidiPrepareBudget {
        bytes: budget.bytes.min(4096),
        scalars: budget.scalars.min(256),
        metadata_ops: budget.metadata_units.min(256),
    };
    // Dispatch through the same sealed representation. The non-Send pending
    // owner stays local to Item while root-level proof reuses its original uniform fact.
    let progress = bidi.prepare_layout_paragraph_range(
        authority,
        payload.cursor,
        budget,
        &mut payload.bidi_visible_preparation,
    )?;
    step.bytes = progress.bytes;
    step.scalars = progress.scalars;
    step.metadata_units = progress.metadata;
    Ok(progress.ready)
}
