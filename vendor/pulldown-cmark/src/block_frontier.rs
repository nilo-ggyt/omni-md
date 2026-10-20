// Copyright 2015 Google Inc. All rights reserved.
// 本地扩展复用固定0.13.4结构规则；完整上游MIT条款保留在LICENSE。

//! 真实global验证后的ActiveTable续行；不保存整行或全部row/cell/text节点。

use crate::structural_spine::StructuralSpineSnapshot;

#[cfg(test)]
use crate::block_context::ContainerRole;
use crate::Options;

/// byte读取与转换均有独立硬上限；成员分类和UTF16计量也必须可恢复。
pub const MAX_FRONTIER_STEP_BYTES: usize = 64 * 1024;
/// 调用者也只能借出有界frame，不能把完整source slice当成分段接口。
pub const MAX_FRONTIER_FRAME_BYTES: usize = 64 * 1024;
/// 与固定依赖原始parse_table_row_inner相同的整表累计补格fuel。
pub const MAX_TABLE_AUTOCOMPLETED_CELLS: usize = crate::table_scan::MAX_AUTOCOMPLETED_CELLS;

/// frame可以在UTF8或CRLF中间结束；只有精确source总长末尾有真实EOF权限。
pub struct SourceFrame<'a> {
    pub source_revision: u64,
    pub byte_start: usize,
    pub bytes: &'a [u8],
    pub real_eof: bool,
}

impl std::fmt::Debug for SourceFrame<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Debug只报告有限坐标/字节数，默认不输出frame正文。
        formatter
            .debug_struct("SourceFrame")
            .field("source_revision", &self.source_revision)
            .field("byte_start", &self.byte_start)
            .field("byte_count", &self.bytes.len())
            .field("real_eof", &self.real_eof)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 不持正文的真实源行事实；ordinal与输入frame/局部Vec位置无关。
pub struct TableRowFact {
    pub header_byte_start: usize,
    pub row_ordinal: usize,
    pub source_line: usize,
    pub byte_start: usize,
    pub byte_end: usize,
    pub utf16_start: usize,
    pub utf16_end: usize,
    pub container_prefix_bytes: usize,
    pub actual_cells: usize,
    pub missing_cells: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 每次返回同时说明消费量；NeedInput与真实Ended具有不同权限。
pub enum TableStep {
    Progress { consumed: usize },
    NeedInput { consumed: usize },
    Row { consumed: usize, fact: TableRowFact },
    Ended { consumed: usize, byte_offset: usize },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 拒绝不被解释为正文fallback成功；错误状态不再消费后继frame。
pub enum FrontierError {
    InvalidFrame,
    InvalidBudget,
    InvalidUtf8,
    MissingCellFuel,
    Closed,
    WorkFuel,
}

#[derive(Clone, Debug)]
/// 只有行边界可checkpoint；多checkpoint共享同一spine，绝不clone当前行正文。
pub struct TableCheckpoint {
    spine: StructuralSpineSnapshot,
    scalars: TableScalars,
    options: Options,
    header_inline: cell::TableInlineRow,
    last_inline: Option<cell::TableInlineRow>,
}

#[derive(Clone, Copy, Debug)]
/// 逐源行推进的结构计数；没有AST索引、容器正文或待发布native资源。
struct TableScalars {
    source_revision: u64,
    source_end: usize,
    header_byte_start: usize,
    separator_byte_start: usize,
    separator_prefix_bytes: usize,
    last_line_terminated: bool,
    columns: usize,
    next_row_ordinal: usize,
    next_source_line: usize,
    next_byte: usize,
    next_utf16: usize,
    missing_cells: usize,
}

/// 主状态仅保共享spine、标量与一个不持正文的未提交行续体。
pub struct TableContinuationState {
    spine: StructuralSpineSnapshot,
    scalars: TableScalars,
    options: Options,
    row: Option<row::ActiveRow>,
    last_work: TableWork,
    ended: Option<usize>,
    failed: bool,
    header_inline: cell::TableInlineRow,
    last_inline: Option<cell::TableInlineRow>,
}

impl std::fmt::Debug for TableContinuationState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 结构状态可审查，scratch正文不进入Debug；这也不创建新正文快照。
        formatter
            .debug_struct("TableContinuationState")
            .field("scalars", &self.scalars)
            .field("spine_depth", &self.spine.len())
            .field("row_active", &self.row.is_some())
            .field("ended", &self.ended)
            .field("failed", &self.failed)
            .finish()
    }
}

pub(crate) mod cell;
mod end;
mod row;
mod scope;
pub use crate::line_frontier::TableDelimiterScanner;
pub use crate::table_scan::{TableCellBoundary, TableCellScanner};
pub use cell::{TableCellSemanticScanner, VerifiedTableInlineRow};
pub use end::VerifiedTableEnd;
pub use row::TableWork;
pub use scope::{TableScopeCursor, TableScopeKind, TableScopeStep};

impl TableContinuationState {
    /// 唯一非test构造入口消耗真实global验证值，不接受任意公开种子字段。
    pub(crate) fn from_verified_global(
        verified: crate::global_frontier::VerifiedGlobalTable,
    ) -> Self {
        let (
            spine,
            source_revision,
            source_end,
            header_byte_start,
            separator_byte_start,
            separator_prefix_bytes,
            last_line_terminated,
            columns,
            next_byte,
            next_utf16,
            next_source_line,
            options,
            header_inline,
        ) = verified.into_parts();
        Self::resume(TableCheckpoint {
            spine,
            scalars: TableScalars {
                source_revision,
                source_end,
                header_byte_start,
                separator_byte_start,
                separator_prefix_bytes,
                last_line_terminated,
                columns,
                next_row_ordinal: 1,
                next_source_line,
                next_byte,
                next_utf16,
                missing_cells: 0,
            },
            options,
            header_inline,
            last_inline: None,
        })
    }

    /// frame锚点可因真实分类前瞻而回放；consumed不是semantic提交量，也不能作为下次offset增量。
    pub fn next_byte(&self) -> usize {
        self.row
            .as_ref()
            .map_or(self.scalars.next_byte, row::ActiveRow::next_byte)
    }
    pub(crate) fn next_source_byte(&self) -> usize {
        self.next_byte()
    }
    pub(crate) fn next_semantic_position(&self) -> (usize, usize) {
        (self.scalars.next_utf16, self.scalars.next_source_line)
    }
    pub fn last_work(&self) -> TableWork {
        self.last_work
    }

    /// checkpoint只含已完成行的真实提交点和共享spine，不保源正文或source强owner。
    pub fn resume(checkpoint: TableCheckpoint) -> Self {
        Self {
            spine: checkpoint.spine,
            scalars: checkpoint.scalars,
            options: checkpoint.options,
            row: None,
            last_work: TableWork::default(),
            ended: None,
            failed: false,
            header_inline: checkpoint.header_inline,
            last_inline: checkpoint.last_inline,
        }
    }
    pub fn checkpoint(&self) -> Option<TableCheckpoint> {
        if self.row.is_some() || self.ended.is_some() || self.failed {
            return None;
        }
        Some(TableCheckpoint {
            spine: self.spine.clone(),
            scalars: self.scalars,
            options: self.options,
            header_inline: self.header_inline,
            last_inline: self.last_inline,
        })
    }
    /// 新实现没有任何行源缓冲；纯metadata/固定游标容量由独立预算计量，不冒称整进程零内存。
    pub fn buffered_source_capacity(&self) -> usize {
        0
    }

    /// 兼容调用者只指定byte上限；转换默认仍受同一64KiB硬上限约束，并由last_work返回。
    pub fn advance(
        &mut self,
        frame: SourceFrame<'_>,
        byte_budget: usize,
    ) -> Result<TableStep, FrontierError> {
        self.advance_bounded(frame, byte_budget, MAX_FRONTIER_STEP_BYTES)
    }
    /// 真正有界的行分类/UTF16计量；每step至少2048转换支付固定游标/收尾，byte预算可为1。
    pub fn advance_bounded(
        &mut self,
        frame: SourceFrame<'_>,
        byte_budget: usize,
        transition_budget: usize,
    ) -> Result<TableStep, FrontierError> {
        if self.failed {
            return Err(FrontierError::Closed);
        }
        if !(2048..=MAX_FRONTIER_STEP_BYTES).contains(&transition_budget) {
            return Err(FrontierError::InvalidBudget);
        }
        crate::source_frontier::admitted_bytes(
            &frame,
            self.scalars.source_revision,
            self.scalars.source_end,
            self.next_byte(),
            byte_budget,
        )?;
        self.last_work = TableWork::default();
        if let Some(byte_offset) = self.ended {
            return Ok(TableStep::Ended {
                consumed: 0,
                byte_offset,
            });
        }
        if self.row.is_none() && self.scalars.next_byte == self.scalars.source_end {
            // 到达声明长度还不等于源owner已交付真实EOF；短frame/尚未完成的读取不能签发表尾权限。
            if !frame.real_eof {
                return Ok(TableStep::NeedInput { consumed: 0 });
            }
            self.ended = Some(self.scalars.next_byte);
            return Ok(TableStep::Ended {
                consumed: 0,
                byte_offset: self.scalars.next_byte,
            });
        }
        let row = self
            .row
            .get_or_insert_with(|| row::ActiveRow::new(self.scalars, &self.spine, self.options));
        let result = match row.advance(frame, byte_budget, transition_budget) {
            Ok(result) => result,
            Err(error) => {
                self.failed = true;
                return Err(error);
            }
        };
        self.last_work = row.last_work();
        debug_assert!(
            self.last_work.byte_reads <= byte_budget
                && self.last_work.transitions <= transition_budget
        );
        let consumed = self.last_work.byte_reads;
        match result {
            row::RowStep::Progress => Ok(TableStep::Progress { consumed }),
            row::RowStep::NeedInput => Ok(TableStep::NeedInput { consumed }),
            row::RowStep::NotMember => {
                // 不消费失败候选的semantic范围；global从原row起点重借并重新分类，不丢后继块。
                self.row = None;
                self.ended = Some(self.scalars.next_byte);
                Ok(TableStep::Ended {
                    consumed,
                    byte_offset: self.scalars.next_byte,
                })
            }
            row::RowStep::Member { line, position } => {
                let missing_cells = self.scalars.columns.saturating_sub(line.actual_cells);
                if self.scalars.missing_cells.saturating_add(missing_cells)
                    > MAX_TABLE_AUTOCOMPLETED_CELLS
                {
                    self.failed = true;
                    return Err(FrontierError::MissingCellFuel);
                }
                let fact = TableRowFact {
                    header_byte_start: self.scalars.header_byte_start,
                    row_ordinal: self.scalars.next_row_ordinal,
                    source_line: self.scalars.next_source_line,
                    byte_start: self.scalars.next_byte,
                    byte_end: line.end,
                    utf16_start: self.scalars.next_utf16,
                    utf16_end: position.utf16,
                    container_prefix_bytes: row.prefix(),
                    actual_cells: line.actual_cells,
                    missing_cells,
                };
                // 只保刚刚实际接受的这一行；源范围与原容器前缀不能由后继调用者的 Range 补造。
                self.last_inline = Some(cell::TableInlineRow {
                    ordinal: fact.row_ordinal,
                    start: fact.byte_start,
                    content_start: fact.byte_start + fact.container_prefix_bytes,
                    content_end: line.content_end,
                    end: fact.byte_end,
                });
                self.scalars.missing_cells += missing_cells;
                self.scalars.next_row_ordinal += 1;
                self.scalars.next_byte = line.end;
                self.scalars.next_utf16 = position.utf16;
                self.scalars.next_source_line = position.line;
                self.scalars.last_line_terminated = line.content_end < line.end;
                self.row = None;
                Ok(TableStep::Row { consumed, fact })
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
