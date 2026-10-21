//! One original word action borrows the current actual cell or restores one cell; query coordinates
//! never replace row/raw/display Source authority.

use super::*;
use omni_markdown_core::{MarkdownComposedLineAffinity, MarkdownWordGroupAnswer};

impl Producer {
    /// Resume the unique cell query, giving Source preparation and word work separate turns without
    /// another read after Yielded.
    pub(super) fn step_cell_word_group(
        &mut self,
        owner: &Owner,
        request: &NativeMarkdownPagedSceneRequest,
    ) -> Result<MarkdownWordGroupAnswer> {
        let ticket =
            PreparationTicket::new(request, &self.source, self.serial, self.width, self.padding);
        ticket.require_pure(owner, &self.source)?;
        if self.cell_word.as_ref().is_some_and(|query| query.cancelled) {
            self.drain_cell_word(owner)?;
            ticket.require(owner, &self.source)?;
            return Ok(MarkdownWordGroupAnswer::Yielded);
        }
        let mut query = self.cell_word.take().expect("原词组仍持唯一格查询");
        let result = self.advance_cell_word_query(owner, &mut query);
        let work = query.take_work();
        self.cell_word = Some(query); // Restore query ownership before Native layout callbacks, preserving preparation occupying the original Request even on failure.
        self.record_grid_work(work)?;
        self.target_source_health()?;
        let answer = match result? {
            Some(answer) => answer,
            None => {
                self.advance(owner, request)?; // Actual header width or another original cell preparation still needs the existing layout slot to advance.
                MarkdownWordGroupAnswer::Yielded
            }
        };
        ticket.require(owner, &self.source)?;
        Ok(answer)
    }

    /// None means this turn did no Source work and the original Producer must first finish actual
    /// font/header/other-cell preparation.
    fn advance_cell_word_query(
        &mut self,
        owner: &Owner,
        query: &mut grid::word::CellWordQuery,
    ) -> Result<Option<MarkdownWordGroupAnswer>> {
        if self.trial.is_some()
            || self.pending_style.is_some()
            || self.pending_control.is_some()
            || self.pending_line.is_some()
            || self.pending_batch.is_some()
            || self.queued_lines.is_some()
        {
            return Ok(None);
        }
        let semantics = self.semantics.as_mut().ok_or(Error::Unsupported)?;
        if !semantics.request_started() || semantics.navigating_cell() {
            return Ok(None);
        }
        if query.recovered() {
            return query
                .advance_recovered(owner, &self.source, semantics)
                .map(Some);
        }
        if self
            .grid
            .iter()
            .chain(self.grids.iter())
            .chain(
                self.restoring_grid
                    .iter()
                    .filter_map(|restoring| restoring.grid.as_ref()),
            )
            .any(|grid| grid.has_preparation() && !grid.word_row(query.target))
            || self
                .restoring_grid
                .as_ref()
                .is_some_and(|restoring| restoring.grid.is_none())
        {
            // Another actual row or restoration without a Grid must return the unique original Request
            // first, even when an existing cell matches.
            // Admit the target row itself because it may own this CellInline continuation; Grid still
            // serializes its other columns.
            return Ok(None);
        }
        let existing = self
            .grid
            .iter_mut()
            .chain(self.grids.iter_mut())
            .chain(
                self.restoring_grid
                    .iter_mut()
                    .filter_map(|restoring| restoring.grid.as_mut()),
            )
            .find(|grid| grid.word_row(query.target));
        if let Some(grid) = existing {
            let result = grid.advance_word(owner, &self.source, query, semantics);
            let work = grid.take_work();
            self.record_grid_work(work)?; // Collect actual Source/grapheme work on both success and failure.
            return result;
        }
        if query.row.is_none() {
            let document = owner
                .document
                .borrow()
                .as_ref()
                .expect("格查询持原Doc")
                .clone();
            let row =
                semantics.table_row(&document.borrow(), &self.source, query.target.row_start)?;
            if let Some(row) = row {
                if row.source_range().start != query.target.row_start
                    || query.target.column >= row.columns()
                {
                    return Err(ViewError::InvalidRange.into());
                }
                query.row = Some(row);
            }
            return Ok(Some(MarkdownWordGroupAnswer::Yielded)); // Actual root replay/publication also consumes this turn original quantum exclusively.
        }
        let row = query.row.as_ref().expect("原最新请求已交真实行");
        if let Some(widths) = self
            .table_widths
            .as_ref()
            .filter(|widths| widths.header == row.coordinate().0)
        {
            let width = *widths
                .widths
                .get(query.target.column)
                .ok_or(ViewError::InvalidRange)?;
            query.restore(width);
            return Ok(Some(MarkdownWordGroupAnswer::Yielded));
        }
        let Some(output) = self.output.as_ref() else {
            return Ok(None); // Without widths for the new canonical Source, continue original header solving instead of using average or old-Source widths.
        };
        if output.borrow().source_range().end <= query.target.row_start {
            return Ok(None);
        }
        if query.lookup.is_none() {
            query.lookup = Some(output.borrow_mut().line_lookup(
                MarkdownComposedLineLookupTarget::Source {
                    utf16: query.target.row_start,
                    affinity: MarkdownComposedLineAffinity::Downstream,
                },
            )?);
            return Ok(Some(MarkdownWordGroupAnswer::Yielded));
        }
        let step = output.borrow_mut().step_line_lookup(
            query.lookup.as_mut().expect("原查询持真实目录续体"),
            METADATA_PER_TURN,
        )?;
        self.step_metadata = self
            .step_metadata
            .checked_add(step.records)
            .ok_or(Error::CounterExhausted)?;
        self.maximum_step_metadata = self.maximum_step_metadata.max(step.records);
        self.target_index_records = self
            .target_index_records
            .checked_add(step.records)
            .ok_or(Error::CounterExhausted)?;
        match step.answer {
            MarkdownComposedLineLookupAnswer::Pending => {}
            MarkdownComposedLineLookupAnswer::PrefixNotCovered => {
                query.lookup = None; // The actual prefix has not reached this row; defer to the original Producer next turn without a second read quantum now.
            }
            MarkdownComposedLineLookupAnswer::Found(line) => {
                if line.facts().source_utf16 != row.source_range() {
                    return Err(ViewError::Revoked.into());
                }
                // As with old-row restoration, read this sealed line column directory only, without
                // document text or invented top/height.
                let mut widths = [0i64; 256];
                let columns = output.borrow_mut().table_column_widths(
                    &self.source.authority(),
                    &line,
                    &mut widths,
                )?;
                if columns != row.columns() {
                    return Err(ViewError::Revoked.into());
                }
                // The actual API reads five part, one metadata, three directory, one header, and one
                // width record per seven columns.
                let records = 10 + columns.div_ceil(7);
                self.step_metadata = self
                    .step_metadata
                    .checked_add(records)
                    .ok_or(Error::CounterExhausted)?;
                self.maximum_step_metadata = self.maximum_step_metadata.max(step.records + records);
                query.restore(widths[query.target.column] as f64 / 64.0);
            }
        }
        Ok(Some(MarkdownWordGroupAnswer::Yielded))
    }

    /// Cancellation drains the actual original CellInline until its slot returns before releasing the
    /// restored cell or starting the registered next head.
    pub(super) fn drain_cell_word(&mut self, owner: &Owner) -> Result<()> {
        let mut query = self.cell_word.take().expect("原格取消仍持实际owner");
        let result = query.drain(
            owner,
            &self.source,
            self.semantics.as_mut().expect("原格来源仍属同请求"),
        );
        let work = query.take_work();
        self.record_grid_work(work)?;
        self.target_source_health()?;
        match result {
            Ok(true) => {
                let restart = query.restart.take();
                drop(query); // Actual preparation returned; drop the old cell before a new begin may borrow the same Request.
                if let Some((action, head, cell)) = restart {
                    self.begin_word_group(action, head, cell)?;
                }
            }
            result => {
                self.cell_word = Some(query);
                result?;
            }
        }
        Ok(())
    }
}
