//! Cell word selection reuses the original display Source and Composer; offscreen recovery owns one
//! actual cell without geometry placeholders.

use super::*;
use crate::native_surface::paged_text_view::NativeMarkdownPagedCellWordTarget;
use omni_markdown_core::{
    MarkdownLayoutTextAuthority, MarkdownWordGroupAction, MarkdownWordGroupAnswer,
};

/// Query scalars and actual recovery resources belong to the Producer unique current word slot, without
/// old Scene/Source continuations.
pub(in crate::native_surface::paged_body::producer) struct CellWordQuery {
    pub target: NativeMarkdownPagedCellWordTarget,
    head: usize,
    action: MarkdownWordGroupAction,
    binding: Option<MarkdownLayoutTextAuthority>,
    pub row: Option<omni_markdown_core::VerifiedMarkdownLayoutTableRow>,
    pub lookup: Option<omni_markdown_core::MarkdownComposedLineLookup>,
    recovered: Option<RecoveredCell>,
    pub cancelled: bool,
    /// While preparation is outstanding, retain at most the next begin intent for the current head
    /// without starting another word query concurrently.
    pub restart: Option<(
        MarkdownWordGroupAction,
        usize,
        Option<NativeMarkdownPagedCellWordTarget>,
    )>,
    work: Work,
}

/// Restore reading for a retired cell using the latest same-request row ticket and actual solved/sealed
/// widths, without invented row geometry.
struct RecoveredCell {
    row: omni_markdown_core::VerifiedMarkdownLayoutTableRow,
    cell: Cell,
}

impl CellWordQuery {
    pub fn new(
        target: NativeMarkdownPagedCellWordTarget,
        head: usize,
        action: MarkdownWordGroupAction,
    ) -> Self {
        Self {
            target,
            head,
            action,
            binding: None,
            row: None,
            lookup: None,
            recovered: None,
            cancelled: false,
            restart: None,
            work: Work::default(),
        }
    }

    pub fn recovered(&self) -> bool {
        self.recovered.is_some()
    }

    /// Actual row metadata certified the width; create a fixed cell owner only, without body Input or
    /// another parser.
    pub fn restore(&mut self, width: f64) {
        self.recovered = Some(RecoveredCell {
            row: self.row.take().expect("恢复格已经取得真实行票"),
            cell: Cell::new(self.target.column, width),
        });
        self.lookup = None;
    }

    pub fn advance_recovered(
        &mut self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        semantics: &mut super::super::super::semantics::RootLeaf,
    ) -> Result<MarkdownWordGroupAnswer> {
        let mut recovered = self.recovered.take().expect("真实恢复格仍由本查询持有");
        let mut work = std::mem::take(&mut self.work);
        // This step performs pure Core Source/grapheme work with no Native callback; restore
        // temporarily moved resources on every result.
        let result = (|| {
            recovered.row.require_for(
                &owner
                    .document
                    .borrow()
                    .as_ref()
                    .expect("格选词持原Doc")
                    .borrow(),
                source,
            )?;
            recovered
                .cell
                .advance_word(owner, source, &recovered.row, self, semantics, &mut work)
        })();
        self.work = work;
        self.recovered = Some(recovered);
        result
    }

    /// Cancelling the business query cannot remove a loaned CellInline; release its cell only after
    /// actual preparation returns.
    pub fn cancel(&mut self) -> bool {
        self.cancelled = true;
        let Some(recovered) = &mut self.recovered else {
            return true;
        };
        if let Some(composer) = &mut recovered.cell.composer {
            composer.cancel_word_group();
        }
        recovered.cell.preparation.is_none()
    }

    /// Continue cancellation through the original semantics slot until its real writer is closed and
    /// returned.
    pub fn drain(
        &mut self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        semantics: &mut super::super::super::semantics::RootLeaf,
    ) -> Result<bool> {
        let Some(recovered) = &mut self.recovered else {
            return Ok(true);
        };
        if recovered.cell.preparation.is_none() {
            return Ok(true); // Unstarted or already-returned preparation has no asynchronous work; cancellation must not start another preparation.
        }
        let document = owner
            .document
            .borrow()
            .as_ref()
            .expect("取消格仍持原Doc")
            .clone();
        let document = document.borrow();
        recovered.row.require_for(&document, source)?;
        let before = semantics.current_work();
        let step = semantics.cancel_cell(
            recovered
                .cell
                .preparation
                .as_mut()
                .expect("只归还实际在途准备"),
            &document,
        )?;
        let (ready, measured) = match step {
            MarkdownTableCellDisplayPreparationStep::Progress { work } => (false, work),
            MarkdownTableCellDisplayPreparationStep::Ready { work } => (true, work),
        };
        self.work
            .display_step(measured, before, semantics.current_work())?;
        if ready {
            recovered.cell.preparation = None; // Core closed the actual writer and returned its slot; only then drop the original task owner.
        }
        Ok(ready)
    }

    pub fn take_work(&mut self) -> Work {
        std::mem::take(&mut self.work)
    }

    pub fn resource_receipt(
        &self,
    ) -> Result<Option<super::super::super::NativeMarkdownPagedCellResourceReceipts>> {
        self.recovered
            .as_ref()
            .map(|recovered| recovered.cell.resource_receipt())
            .transpose()
    }
}

impl Grid {
    /// Only the actual current row can lend its cells; preserve the queried column instead of inferring
    /// missing-cell ownership from an empty range.
    pub(in crate::native_surface::paged_body::producer) fn word_row(
        &self,
        target: NativeMarkdownPagedCellWordTarget,
    ) -> bool {
        self.source_range().start == target.row_start
    }

    pub(in crate::native_surface::paged_body::producer) fn advance_word(
        &mut self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        query: &mut CellWordQuery,
        semantics: &mut super::super::super::semantics::RootLeaf,
    ) -> Result<Option<MarkdownWordGroupAnswer>> {
        self.require(owner, source)?;
        if query.target.column >= self.row.columns() {
            return Err(ViewError::InvalidRange.into());
        }
        if self.natural.is_some() {
            return Ok(None); // Finish original header solving before final widths exist; temporary width-measurement Composers cannot certify word groups.
        }
        if self
            .cells
            .iter()
            .any(|cell| cell.column != query.target.column && cell.preparation.is_some())
        {
            return Ok(None); // Another column still occupies the unique original Source request; the outer owner must advance its return first.
        }
        let cell = &mut self.cells[query.target.column];
        cell.advance_word(owner, source, &self.row, query, semantics, &mut self.work)
            .map(Some)
    }

    pub(in crate::native_surface::paged_body::producer) fn cancel_word(
        &mut self,
        query: &CellWordQuery,
    ) {
        if !self.word_row(query.target)
            || self
                .natural
                .as_ref()
                .is_some_and(|natural| natural.active.column != query.target.column)
        {
            return;
        }
        if let Some(composer) = self.cell_mut(query.target.column).composer.as_mut() {
            composer.cancel_word_group();
        }
    }
}

impl Cell {
    /// Display preparation, grapheme word queries, and actual mapping serially reuse the same cell
    /// without requiring all glyphs or LineReady.
    fn advance_word(
        &mut self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        row: &omni_markdown_core::VerifiedMarkdownLayoutTableRow,
        query: &mut CellWordQuery,
        semantics: &mut super::super::super::semantics::RootLeaf,
        work: &mut Work,
    ) -> Result<MarkdownWordGroupAnswer> {
        if self.empty.is_none() && self.composer.is_none() {
            self.advance(owner, source, row, false, semantics, work)?;
            return Ok(MarkdownWordGroupAnswer::Yielded);
        }
        let document = owner
            .document
            .borrow()
            .as_ref()
            .expect("格选词持原Doc")
            .clone();
        let document = document.borrow();
        if let Some(empty) = &self.empty {
            let range = empty.canonical_range_for(&document)?;
            if empty.coordinate_for(&document)?.2 != query.target.column
                || !range.is_empty()
                || query.head != range.start
            {
                return Err(ViewError::InvalidRange.into());
            }
            return Ok(MarkdownWordGroupAnswer::Ready(range.clone())); // The verified empty cell and empty range certify an actual empty word group without invented glyphs.
        }
        let display = self.display.as_ref().expect("非空格Composer持原显示源");
        display.require_for(&document)?;
        let raw = display.canonical_raw_cell();
        if raw.coordinate_for(&document)?.2 != query.target.column {
            return Err(ViewError::Revoked.into());
        }
        let authority = display.authority();
        let composer = self.composer.as_mut().expect("原格Composer已经准备");
        if let Some(original) = &query.binding {
            if *original != authority {
                return Err(ViewError::Revoked.into()); // A started query cannot renew authority from a successor display Source.
            }
        } else {
            let affinity = if query.target.affinity.is_upstream() {
                omni_markdown_core::MarkdownComposedLineAffinity::Upstream
            } else {
                omni_markdown_core::MarkdownComposedLineAffinity::Downstream
            };
            let head = display
                .canonical_to_display(query.head, affinity)
                .map_err(ViewError::Document)?
                .ok_or(ViewError::InvalidRange)?;
            composer.begin_word_group(query.action, head)?;
            query.binding = Some(authority);
        }
        // Use the same bounded Composer cursor for this original display authority, never a copied cell
        // string.
        let before = composer.frontier_work()?;
        let step = composer.step_word_group(MarkdownLineComposeBudget {
            bytes: 4096,
            scalars: 256,
            records: 256,
            metadata_units: 256,
        })?;
        work.word_step(&step, before, composer.frontier_work()?)?;
        display.require_for(&document)?;
        let map = |range: Range<usize>| -> Result<Range<usize>> {
            if range.is_empty() {
                // An empty display group remains one original insertion point; hidden markers on either
                // side must not map it into a reversed range.
                let affinity = if query.target.affinity.is_upstream() {
                    omni_markdown_core::MarkdownComposedLineAffinity::Upstream
                } else {
                    omni_markdown_core::MarkdownComposedLineAffinity::Downstream
                };
                let position = display
                    .display_to_canonical(range.start, affinity)
                    .map_err(ViewError::Document)?;
                return Ok(position..position);
            }
            Ok(display
                .display_to_canonical(
                    range.start,
                    omni_markdown_core::MarkdownComposedLineAffinity::Downstream,
                )
                .map_err(ViewError::Document)?
                ..display
                    .display_to_canonical(
                        range.end,
                        omni_markdown_core::MarkdownComposedLineAffinity::Upstream,
                    )
                    .map_err(ViewError::Document)?)
        };
        let answer = match step.answer {
            MarkdownWordGroupAnswer::Ready(range) => MarkdownWordGroupAnswer::Ready(map(range)?),
            MarkdownWordGroupAnswer::NeedsFoldExpansion(range) => {
                MarkdownWordGroupAnswer::NeedsFoldExpansion(map(range)?)
            }
            answer => answer,
        };
        display.require_for(&document)?;
        Ok(answer)
    }
}

impl Work {
    /// Word and layout steps share actual work accounting; add frontier deltas only, without
    /// duplicating parent semantic reads.
    fn word_step(
        &mut self,
        step: &omni_markdown_core::MarkdownWordGroupStep,
        before: omni_markdown_core::MarkdownLineFrontierWork,
        after: omni_markdown_core::MarkdownLineFrontierWork,
    ) -> Result<()> {
        let frontier = after
            .source_bytes
            .checked_sub(before.source_bytes)
            .ok_or(Error::CounterExhausted)?;
        let shaped = after
            .input_bytes
            .checked_sub(before.input_bytes)
            .ok_or(Error::CounterExhausted)?;
        for (total, value) in [
            (&mut self.bytes, step.bytes),
            (&mut self.scalars, step.scalars),
            (&mut self.metadata, step.metadata_units),
            (&mut self.frontier_source_bytes, frontier),
            (&mut self.shaped_input_bytes, shaped),
        ] {
            *total = total.checked_add(value).ok_or(Error::CounterExhausted)?;
        }
        self.maximum_bytes = self.maximum_bytes.max(step.bytes);
        self.maximum_scalars = self.maximum_scalars.max(step.scalars);
        self.maximum_metadata = self.maximum_metadata.max(step.metadata_units);
        Ok(())
    }
}
