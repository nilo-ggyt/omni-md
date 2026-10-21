//! Prepare whole-source bidi through the coordinator's sole owner. Layout and
//! word navigation share its original cancellation state and work quantum.

use super::*;

/// Construct and advance the actual original bidi preparation, retaining its
/// observation even when construction or a later step fails.
pub(super) fn advance(
    payload: &mut Payload,
    authority: &MarkdownLayoutTextAuthority,
    cancellation: &MarkdownLineComposeCancellation,
    budget: MarkdownLineComposeBudget,
    step: &mut MarkdownLineItemStep,
) -> Result<()> {
    if payload.bidi_preparation.is_none() {
        // Mark observation coverage before anonymous files/pages can be acquired;
        // a failed constructor still returns its original resource observation.
        payload.resources.begin_bidi();
        let memory = BidiMemoryBudget::new(64 * 1024)?;
        #[cfg(feature = "paged-shaping-frontier")]
        let (preparation, observer) =
            observed_preparation(payload, authority, cancellation, memory);
        #[cfg(not(feature = "paged-shaping-frontier"))]
        let (preparation, observer) = MarkdownBidiPrepareJob::new_layout_source_observed(
            payload.source.clone(),
            authority,
            BidiBaseDirection::Auto,
            memory,
            &cancellation.bidi,
        );
        payload.resources.register_bidi(observer);
        payload.bidi_preparation = Some(preparation?);
    }
    let preparation = payload
        .bidi_preparation
        .as_mut()
        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
    // Charge only this dependency's actual source and metadata work, never
    // reusing the same quantum for a second itemization stage in this call.
    let progress = preparation.step(
        authority,
        MarkdownBidiPrepareBudget {
            bytes: budget.bytes.min(4096),
            scalars: budget.scalars,
            metadata_ops: budget.metadata_units.min(METADATA_UNITS),
        },
    )?;
    step.bytes = progress.bytes;
    step.scalars = progress.scalars;
    step.metadata_units = progress.metadata_ops;
    if progress.status == MarkdownBidiPrepareStatus::Ready {
        // Transfer the original paragraph only after real EOF, complete rules,
        // and actual flush; Pending grants no resolved-level authority.
        payload.bidi = Some(
            preparation
                .take_ready(authority)?
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?,
        );
        payload.bidi_preparation.take();
    }
    Ok(())
}

/// Select the original bidi entry by explicit display authority within the same
/// composition. A Source without table proof cannot enter table-aware preparation.
#[cfg(feature = "paged-shaping-frontier")]
fn observed_preparation(
    payload: &Payload,
    authority: &MarkdownLayoutTextAuthority,
    cancellation: &MarkdownLineComposeCancellation,
    memory: BidiMemoryBudget,
) -> (
    std::result::Result<MarkdownBidiPrepareJob, BidiError>,
    crate::MarkdownBidiResourceObserver,
) {
    #[cfg(feature = "s2-table-window-prototype")]
    if let Some(tables) = payload.tables.as_ref().map(|writer| writer.plan()) {
        // The original table plan and optional fold proof jointly identify
        // visible paragraphs without synthesizing text across hidden intervals.
        return MarkdownBidiPrepareJob::new_layout_source_table_observed(
            payload.source.clone(),
            authority,
            tables,
            payload.folds.shared(),
            BidiBaseDirection::Auto,
            memory,
            &cancellation.bidi,
        );
    }
    if let Some(folds) = payload.folds.shared() {
        // Folding alone still uses its original shared proof and source identity.
        MarkdownBidiPrepareJob::new_layout_source_folded_observed(
            payload.source.clone(),
            authority,
            folds,
            BidiBaseDirection::Auto,
            memory,
            &cancellation.bidi,
        )
    } else {
        // An ordinary source needs neither a replacement plan nor a display copy.
        MarkdownBidiPrepareJob::new_layout_source_observed(
            payload.source.clone(),
            authority,
            BidiBaseDirection::Auto,
            memory,
            &cancellation.bidi,
        )
    }
}
