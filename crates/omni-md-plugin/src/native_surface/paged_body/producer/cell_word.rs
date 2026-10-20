//! 同一原词组动作只借当前真格或恢复一个真格；查询位置从不代替 Row、raw 或显示源权限。

use super::*;
use omni_markdown_core::{MarkdownComposedLineAffinity, MarkdownWordGroupAnswer};

impl Producer {
    /// 接回当前唯一格查询。来源准备和词组各占一个回合，不在 Yielded 后偷做第二份读取。
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
        self.cell_word = Some(query); // Native布局回调前归还owner；失败也不能丢仍占原Request的准备。
        self.record_grid_work(work)?;
        self.target_source_health()?;
        let answer = match result? {
            Some(answer) => answer,
            None => {
                self.advance(owner, request)?; // 真实表头宽度/原其他Cell准备尚未交回，沿原布局槽推进。
                MarkdownWordGroupAnswer::Yielded
            }
        };
        ticket.require(owner, &self.source)?;
        Ok(answer)
    }

    /// None 仅表示本步尚未做来源工作，需要原Producer先结束真实字体/表头/其他格准备。
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
        let semantics = self.semantics.as_mut().ok_or_else(|| unsupported())?;
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
            // 另一真实行或尚未取得Grid的恢复任务先归还原唯一Request，existing命中也不能绕过。
            // 目标行自身必须放行：它可能正持本次要续的CellInline；同行其他列仍由Grid串行推进。
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
            self.record_grid_work(work)?; // 成功或失败都接回这次真实来源/GCB费用。
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
            return Ok(Some(MarkdownWordGroupAnswer::Yielded)); // 真正根重放/发布亦独占本步原量子。
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
            return Ok(None); // 新canonical尚无该表宽度；继续原表头解算，不用平均宽或旧Source宽。
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
                query.lookup = None; // 实际prefix尚未到行；下回合再交原Producer，不能在本步另读第二份量子。
            }
            MarkdownComposedLineLookupAnswer::Found(line) => {
                if line.facts().source_utf16 != row.source_range() {
                    return Err(ViewError::Revoked.into());
                }
                // 与原旧行恢复相同，只读这条封存行的列目录；不读取正文或计算一个假top/height。
                let mut widths = [0i64; 256];
                let columns = output.borrow_mut().table_column_widths(
                    &self.source.authority(),
                    &line,
                    &mut widths,
                )?;
                if columns != row.columns() {
                    return Err(ViewError::Revoked.into());
                }
                // 原API实际读5条part、1条meta、3条directory、1条header和每7列1条width。
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

    /// 取消继续原实际CellInline直到take归槽，随后才释放恢复Cell或开始已登记的当前队首。
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
                drop(query); // 实际准备已交回，先释放旧Cell，再允许新begin借同一Request。
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
