//! 格内选词复用原 Cell 的显示源和 Composer；离屏恢复只有一个实际 Cell，不建立几何占位。

use super::*;
use crate::native_surface::paged_text_view::NativeMarkdownPagedCellWordTarget;
use omni_markdown_core::{
    MarkdownLayoutTextAuthority, MarkdownWordGroupAction, MarkdownWordGroupAnswer,
};

/// 查询标量和实际恢复资源同属 Producer 当前一个词组槽；旧 Scene/Source 不进入续体。
pub(in crate::native_surface::paged_body::producer) struct CellWordQuery {
    pub target: NativeMarkdownPagedCellWordTarget,
    head: usize,
    action: MarkdownWordGroupAction,
    binding: Option<MarkdownLayoutTextAuthority>,
    pub row: Option<omni_markdown_core::VerifiedMarkdownLayoutTableRow>,
    pub lookup: Option<omni_markdown_core::MarkdownComposedLineLookup>,
    recovered: Option<RecoveredCell>,
    pub cancelled: bool,
    /// 原准备尚未交回时，最多保当前队首下一次 begin 的固定意图，绝不并行启动第二个词组。
    pub restart: Option<(
        MarkdownWordGroupAction,
        usize,
        Option<NativeMarkdownPagedCellWordTarget>,
    )>,
    work: Work,
}

/// 只为已退休格恢复读源；行票来自最新同请求，宽度来自原解算器或封存目录，没有假行框。
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

    /// 宽度已经由真正表行认证；这里只创建固定 Cell owner，不创建正文输入或另一解析器。
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
        // 本步只有纯Core来源/GCB操作，没有Native回调；暂借字段后无论成功失败都归还实际资源。
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

    /// 删除业务查询不能撤掉已经借出的 CellInline。只有真实准备交回后才释放这个 Cell。
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
            return Ok(true); // 未启动或已经交还的来源没有异步工作，不能为了取消再开一次准备。
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
            recovered.cell.preparation = None; // Core已经关闭真实writer并归槽，此处才丢原任务owner。
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
    /// 当前真实行才可借其格；column 是原命中查询，不根据零长范围猜隐式缺格归属。
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
            return Ok(None); // 真正最终宽度尚未发布，先完成原表头解算，不借临时测宽Composer签词组。
        }
        if self
            .cells
            .iter()
            .any(|cell| cell.column != query.target.column && cell.preparation.is_some())
        {
            return Ok(None); // 原唯一来源请求仍被其他列占用，外层先推进其准备归还。
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
    /// 显示准备、GCB词组和实际映射串行复用同一 Cell；选词不要求整格字形或 LineReady。
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
            return Ok(MarkdownWordGroupAnswer::Ready(range.clone())); // 双空证明授真实空组，未制造字形。
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
                return Err(ViewError::Revoked.into()); // 已开始查询不能用后继显示Source补权。
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
                // 真空显示上的组仍是一个原插入点；两侧隐藏标记不能把空组映成倒置范围。
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
    /// 词组步进与排版步进记同一真实计量；前沿只累计本步增量，不重复加父请求的语义读量。
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
