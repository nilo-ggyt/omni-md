//! Hand off tables as actual blocks in the original source flow, using the
//! existing style nonce rather than another Unicode worker.
use super::*;
use crate::VerifiedMarkdownLayoutTableRow;
use crate::markdown_layout_source::table_rows::{TablePlan, TablePlanWriter};

/// At a sealed row start during same-source restyling, request the current
/// semantic row ticket before resuming the original table directory. This is
/// structural style scope, with no invented raw-text script/bidi/grapheme/font facts.
pub(super) fn known_row_request(
    payload: &mut Payload,
    authority: &MarkdownLayoutTextAuthority,
    step: &mut MarkdownLineItemStep,
) -> Result<bool> {
    let Some(tables) = payload.tables.as_ref() else {
        // This source has no original table-plan owner to query.
        return Ok(false);
    };
    if payload.table_checked_at == Some(payload.cursor) {
        // The same cursor's directory access was already charged; let its next
        // original input phase proceed without repeating that metadata work.
        return Ok(false);
    }
    let (span, records) = tables.peek_published(&payload.source)?;
    payload.table_checked_at = Some(payload.cursor);
    step.records = records;
    // Even an empty plan examines its original publication slot, but no pread
    // means no fictional record may be charged.
    step.metadata_units = 1;
    let Some(span) = span else {
        // Actual directory access ended this quantum without a published row.
        return Ok(true);
    };
    if span.start.utf16 < payload.cursor {
        // A previously passed row cannot be replayed behind the canonical cursor.
        return Err(MarkdownLineComposeError::InconsistentCoverage);
    }
    if span.start.utf16 != payload.cursor {
        // The text cursor has not reached that row. This directory access
        // consumed its quantum; original body preparation resumes next step.
        return Ok(true);
    }
    let request = MarkdownLineStyleRequest {
        nonce: Arc::new(()),
        source: authority.clone(),
        shaping_seams: [None, None],
        range: span.start.utf16..span.end.utf16,
        scope: StyleScope::Table(span),
        continuation_font: None,
        continuation_bytes: None,
    };
    payload.request = Some(request.clone());
    payload.stage = Stage::Style;
    step.answer = MarkdownLineItemAnswer::NeedStyle(request);
    Ok(true)
}

impl MarkdownLineItemCoordinator {
    /// Share an initially empty directory at construction. As viewports discover
    /// rows, original Unicode owners consume the same actual structural records.
    pub(in crate::markdown_line_composer) fn install_table_flow(
        &mut self,
        writer: TablePlanWriter,
    ) -> Result<()> {
        self.guard()?;
        let payload = self
            .payload
            .as_mut()
            .ok_or(MarkdownLineComposeError::Poisoned)?;
        if !matches!(payload.stage, Stage::Bidi) || payload.cursor != 0 {
            // A consumed source cannot retroactively acquire different table structure.
            return Err(MarkdownLineComposeError::InvalidRequest);
        }
        let plan = writer.plan();
        payload
            .resources
            .register_table_directory(plan.resource_observer());
        payload.scripts.install_table_plan(plan.clone())?;
        payload.breaks.install_table_plan(plan.clone())?;
        payload.tables = Some(writer);
        Ok(())
    }

    /// Preserve the original structural directory and language owners during
    /// restyling; the replacement glyph writer still belongs to this canonical Source.
    pub(in crate::markdown_line_composer) fn table_plan(&self) -> Option<TablePlan> {
        self.payload
            .as_ref()
            .and_then(|payload| payload.tables.as_ref().map(TablePlanWriter::plan))
    }

    /// Accept only the current outstanding request. Old nonces, foreign sources,
    /// and positions other than an actual physical row start cannot become table controls.
    pub(in crate::markdown_line_composer) fn begin_table_row(
        &mut self,
        request: &MarkdownLineStyleRequest,
        row: &VerifiedMarkdownLayoutTableRow,
    ) -> Result<()> {
        self.guard()?;
        let payload = self
            .payload
            .as_mut()
            .ok_or(MarkdownLineComposeError::Poisoned)?;
        let pending = payload
            .request
            .as_ref()
            .ok_or(MarkdownLineComposeError::InvalidRequest)?;
        if !matches!(payload.stage, Stage::Style)
            || !Arc::ptr_eq(&pending.nonce, &request.nonce)
            || pending.source != request.source
            || request.source != self.authority
            || !payload.source.permits(row.source_authority())
            || payload.cursor != row.source_range().start
            || request.range.start != payload.cursor
            || payload.item.is_some()
            || payload.closed_input.is_some()
            || payload.closed_fonts.is_some()
            || payload.frontier.is_some()
        {
            // Row handoff cannot coexist with another active glyph/input/font
            // owner or substitute coordinates for the actual original source ticket.
            return Err(MarkdownLineComposeError::SourceMismatch);
        }
        if let StyleScope::Table(span) = &pending.scope
            && *span != row.span()
        {
            // The original directory identifies which row needs reacquisition;
            // the current semantic publication must return that exact structural span.
            return Err(MarkdownLineComposeError::SourceMismatch);
        }
        // Transfer only the original nonce now. Actual directory append/suffix
        // flush belongs to later budgeted advance. This request created no shape
        // job, so revoke only its probe/scan shell, not already committed glyphs.
        payload.font_probe_scope = None;
        payload.request = None;
        payload.scan = None;
        payload.open_probe = None;
        payload.pending_style = None;
        payload.stage = Stage::TableDirectory;
        Ok(())
    }

    /// Advance one original dependency per step, charging actual trailing CR/LF
    /// bytes without parsing table interiors as body script or grapheme input.
    pub(in crate::markdown_line_composer) fn advance_table_row(
        &mut self,
        row: &VerifiedMarkdownLayoutTableRow,
        budget: MarkdownLineComposeBudget,
    ) -> Result<(MarkdownLineItemStep, bool)> {
        self.guard()?;
        let payload = self
            .payload
            .as_mut()
            .ok_or(MarkdownLineComposeError::Poisoned)?;
        if !matches!(payload.stage, Stage::TableDirectory | Stage::TableRow)
            || !payload.source.permits(row.source_authority())
        {
            // Only the same original row-source proof can drive this pending handoff.
            return Err(MarkdownLineComposeError::SourceMismatch);
        }
        let end = row.source_range().end;
        let mut result = MarkdownLineItemStep {
            bytes: 0,
            context_bytes: 0,
            scalars: 0,
            records: 0,
            metadata_units: 0,
            shapes: 0,
            answer: MarkdownLineItemAnswer::Yielded,
        };
        if matches!(payload.stage, Stage::TableDirectory) {
            // Finish real structural publication before letting another Unicode
            // owner consume its announced span.
            let progress = payload
                .tables
                .as_mut()
                .ok_or(MarkdownLineComposeError::InvalidRequest)?
                .advance_row(&payload.source, row)?;
            result.records = progress.records;
            result.metadata_units = progress.metadata_units;
            if progress.ready {
                payload.scripts.table_announced();
                payload.stage = Stage::TableRow;
            }
            // Directory access/publication already consumed this quantum; do
            // not spend it again advancing another original Unicode owner.
            return Ok((result, false));
        }
        if !payload.scripts.table_passed(end) {
            // Script consumes the original structural jump and any actual line
            // suffix under its own byte/scalar/metadata allowance.
            let step = payload.scripts.step(
                &self.authority,
                MarkdownScriptBudget {
                    bytes: budget.bytes,
                    scalars: budget.scalars,
                    records: budget.records,
                    stack_ops: budget.metadata_units,
                },
            )?;
            result.bytes = step.bytes;
            result.scalars = step.scalars;
            result.records = step.records;
            result.metadata_units = step.stack_ops;
            return Ok((result, false));
        }
        if !payload.breaks.table_passed(&self.authority, end)? {
            // Break indexes independently pass that same actual row span before
            // table geometry can be published as complete composition.
            let step = payload.breaks.step(
                &self.authority,
                MarkdownLineBreakBudget {
                    bytes: budget.bytes,
                    scalars: budget.scalars,
                    records: budget.records,
                },
            )?;
            result.bytes = step.bytes;
            result.scalars = step.scalars;
            result.records = step.records;
            return Ok((result, false));
        }
        Ok((result, true))
    }

    /// Advance canonical coordinates only after the actual row writer committed
    /// its LineEnd; resume subsequent body text through the original ItemStart path.
    pub(in crate::markdown_line_composer) fn finish_table_row(
        &mut self,
        row: &VerifiedMarkdownLayoutTableRow,
    ) -> Result<()> {
        self.guard()?;
        let payload = self
            .payload
            .as_mut()
            .ok_or(MarkdownLineComposeError::Poisoned)?;
        if !matches!(payload.stage, Stage::TableRow) || payload.cursor != row.source_range().start {
            // A row may finish only once at its original unconsumed starting position.
            return Err(MarkdownLineComposeError::InvalidRequest);
        }
        payload.cursor = row.source_range().end;
        payload.stage = Stage::ItemStart;
        Ok(())
    }
}
