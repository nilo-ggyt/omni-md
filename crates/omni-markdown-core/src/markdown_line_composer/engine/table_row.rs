//! Hand the original Style request at a real structural row start to the same Grid.
//! Commit the block to the original line directory only after final geometry returns.
use super::*;
use crate::VerifiedMarkdownLayoutTableRow;
use crate::markdown_layout_source::table_rows::TablePlanWriter;

/// Retains the real structural publication and weak Source authority. Cloning
/// creates no text copy, cell-editing session, or second worker.
#[derive(Clone)]
pub struct MarkdownLineTableRowRequest {
    nonce: std::sync::Arc<()>,
    row: VerifiedMarkdownLayoutTableRow,
    source: MarkdownLayoutTextAuthority,
}
/// Geometry returned after the Grid completes all real cells. Column widths are
/// sealed 26.6 values for complete cell frames.
pub struct MarkdownLineTableRowResponse {
    request: MarkdownLineTableRowRequest,
    registered: MarkdownLineRegisteredFont,
    geometry: TableGeometry,
}
pub(in crate::markdown_line_composer) struct TableGeometry {
    pub(in crate::markdown_line_composer) width: i64,
    pub(in crate::markdown_line_composer) height: i64,
    pub(in crate::markdown_line_composer) columns: Box<[i64]>,
}
pub(super) struct PendingRow {
    request: MarkdownLineTableRowRequest,
    response: Option<MarkdownLineTableRowResponse>,
}
impl MarkdownLineTableRowRequest {
    /// The original sealed row owns cell factories and canonical Source provenance.
    /// Caller coordinates cannot manufacture another adjacent row.
    pub fn row(&self) -> &VerifiedMarkdownLayoutTableRow {
        &self.row
    }
    /// The font preserves actual same-palette liveness; a block has no text point
    /// size. Width/height come from the completed Grid, not a guessed row height.
    pub fn respond_registered(
        self,
        registered: &MarkdownLineRegisteredFont,
        height_26_6: i64,
        width_26_6: i64,
        column_widths_26_6: &[i64],
    ) -> Result<MarkdownLineTableRowResponse> {
        // Require positive complete frames and the actual structural column count.
        // Their checked sum must equal the reported Grid width before transfer.
        if height_26_6 <= 0
            || width_26_6 <= 0
            || column_widths_26_6.len() != self.row.columns()
            || column_widths_26_6.len() > 256
            || column_widths_26_6.iter().any(|width| *width <= 0)
            || column_widths_26_6
                .iter()
                .try_fold(0i64, |total, width| total.checked_add(*width))
                != Some(width_26_6)
        {
            return Err(MarkdownLineComposeError::InvalidRequest);
        }
        registered.registration().lease()?;
        Ok(MarkdownLineTableRowResponse {
            request: self,
            registered: registered.clone(),
            geometry: TableGeometry {
                width: width_26_6,
                height: height_26_6,
                columns: column_widths_26_6.into(),
            },
        })
    }
}
impl MarkdownLineComposer {
    /// Keep original frontier text composition and share its initially empty table
    /// plan. Plain Source/active-cell construction does not gain table-flow authority.
    pub fn new_with_table_flow(
        source: MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        fonts: MarkdownLineFontCatalog,
        width_26_6: i64,
        cancellation: MarkdownLineComposeCancellation,
    ) -> Result<Self> {
        let mut result =
            Self::new_with_frontier(source, authority, fonts, width_26_6, cancellation)?;
        let payload = result
            .payload
            .as_mut()
            .ok_or(MarkdownLineComposeError::Poisoned)?;
        let writer = TablePlanWriter::new(&payload.source);
        payload.store.set_table_plan(Some(writer.plan()));
        payload.coordinator.install_table_flow(writer)?;
        Ok(result)
    }

    /// Begin only after the original text line completes. The current Style nonce
    /// transfers this actual physical row to Grid without discarding an active Measure.
    pub fn begin_table_row(
        &mut self,
        request: &MarkdownLineStyleRequest,
        row: VerifiedMarkdownLayoutTableRow,
    ) -> Result<MarkdownLineTableRowRequest> {
        let result = (|| {
            self.guard()?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            // A structural block starts only at the composer's current committed
            // endpoint, with no in-flight text, resolver, or previous table owner.
            if !matches!(payload.stage, Stage::Style)
                || payload.start != row.source_range().start
                || payload.measure.is_some()
                || payload.current.is_some()
                || payload.next.is_some()
                || payload.item.is_some()
                || payload.resolver.is_some()
                || payload.graphemes.is_some()
                || payload.table.is_some()
            {
                return Err(MarkdownLineComposeError::InvalidRequest);
            }
            payload.coordinator.begin_table_row(request, &row)?;
            let request = MarkdownLineTableRowRequest {
                nonce: std::sync::Arc::new(()),
                row,
                source: self.authority.clone(),
            };
            payload.request = None;
            payload.next_start = None; // A text LineEnd start ticket cannot cross this block.
            payload.table = Some(PendingRow {
                request: request.clone(),
                response: None,
            });
            payload.stage = Stage::TableRow;
            Ok(request)
        })();
        self.finish(result)
    }

    /// An old row response cannot answer a new nonce at equal coordinates. The
    /// original Store also rechecks actual registration and Source during capture.
    pub fn provide_table_row(&mut self, response: MarkdownLineTableRowResponse) -> Result<()> {
        let result = (|| {
            self.guard()?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            let pending = payload
                .table
                .as_mut()
                .ok_or(MarkdownLineComposeError::InvalidRequest)?;
            // Accept one response for this original pending row, then verify the
            // live registration before retaining its completed geometry.
            if !matches!(payload.stage, Stage::TableRow)
                || pending.response.is_some()
                || !std::sync::Arc::ptr_eq(&pending.request.nonce, &response.request.nonce)
                || response.request.source != self.authority
            {
                return Err(MarkdownLineComposeError::SourceMismatch);
            }
            response.registered.registration().lease()?;
            pending.response = Some(response);
            Ok(())
        })();
        self.finish(result)
    }
}

/// Advance only one of original dependencies, graphemes, item capture, or four
/// block-line records per turn. An unfinished Grid never advances the text cursor.
pub(super) fn advance(
    payload: &mut Payload,
    authority: &MarkdownLayoutTextAuthority,
    budget: MarkdownLineComposeBudget,
    step: &mut MarkdownLineComposeStep,
) -> Result<()> {
    let pending = payload
        .table
        .as_mut()
        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
    match payload.stage {
        Stage::TableRow => {
            if pending.response.is_none() {
                // Waiting on real Grid completion grants no text/GCB progress.
                step.answer = MarkdownLineComposeAnswer::NeedsTableRow(pending.request.clone());
                return Ok(());
            }
            let (progress, ready) = payload
                .coordinator
                .advance_table_row(&pending.request.row, budget)?;
            step.bytes = progress.bytes;
            step.scalars = progress.scalars;
            step.records = progress.records;
            step.metadata_units = progress.metadata_units;
            if ready {
                payload.store.set_table_skip(pending.request.row.span());
                payload.stage = Stage::TableGraphemes;
            }
        }
        Stage::TableGraphemes => {
            // Preserve the same canonical row span while migrating only its visible
            // structural endpoints into the original grapheme record domain.
            let progress = payload.store.capture_graphemes_through(
                payload.coordinator.word_breaks()?,
                pending.request.row.source_range().end,
                budget.records.min(256),
                budget.metadata_units.min(256),
            )?;
            step.records = progress.records;
            step.metadata_units = progress.metadata_units;
            if progress.ready {
                let response = pending
                    .response
                    .take()
                    .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                let range = pending.request.row.source_range();
                let value = super::super::controls::ValidatedControl {
                    request: MarkdownLineInlineControl::new(
                        authority.clone(),
                        range.clone(),
                        MarkdownLineInlineControlKind::TableRow,
                    ),
                    registered: response.registered,
                    // This independent block's range is its actual layout domain;
                    // multiple physical lines must not masquerade as a UBA paragraph.
                    facts: MarkdownLineItemFacts {
                        source_utf16: range.clone(),
                        paragraph_utf16: range,
                        script: *b"Zyyy",
                        level: 0,
                        direction: crate::MarkdownShapingDirection::LeftToRight,
                        points: 0.0,
                        beginning_of_text: true,
                        end_of_text: true,
                    },
                    paragraph: MarkdownLineParagraphStyle {
                        tab_stops: Vec::new(),
                        default_tab_interval_26_6: None,
                    },
                    presentation: crate::MarkdownPresentationStyles::default(),
                    paragraph_inset_26_6: 0,
                    table: Some(response.geometry),
                };
                payload.store.begin_control(value)?;
                payload.stage = Stage::TableCapture;
            }
        }
        Stage::TableCapture => {
            // Capture the real control item first, then derive the block's sealed
            // line records from that same opaque item proof.
            let before = payload.store.record_count()?;
            if let Some(proof) = payload.store.step_control(budget.records.min(256))? {
                payload.store.begin_table_line(&proof)?;
                payload.stage = Stage::TablePublish;
            }
            step.records = payload
                .store
                .record_count()?
                .checked_sub(before)
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
        }
        Stage::TablePublish => {
            // Only actual LineEnd completion permits the original coordinator to
            // finish the row and move the public composition cursor.
            let before = payload.store.record_count()?;
            if let Some(line) = payload.store.step_table_line(budget.records.min(256))? {
                payload.coordinator.finish_table_row(&pending.request.row)?;
                payload.start = line.source_utf16.end;
                payload.table = None;
                payload.stage = Stage::Start;
                step.answer = MarkdownLineComposeAnswer::LineReady(line);
            }
            step.records = payload
                .store
                .record_count()?
                .checked_sub(before)
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
        }
        _ => return Err(MarkdownLineComposeError::InvalidRequest),
    }
    Ok(())
}

#[cfg(test)]
mod tests;
