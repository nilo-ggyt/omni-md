// Copyright 2015 Google Inc. All rights reserved.
// 原 TableCell 词法和数学状态沿固定 pulldown-cmark 0.13.4；MIT 条款见 LICENSE。
//! 原已接受行到单格 inline seed 的所有权交接；不持正文，也不把结构完成当成样式完成。

use super::*;
use crate::global_frontier::{InlineBlockKind, VerifiedInlineSemanticSeed};
use crate::math_context::{observe_math_byte, MathContext};
use std::ops::Range;

#[derive(Clone, Copy, Debug)]
/// 只在真实 H 或 Row 接受点创建；行内容末端与 EOL 分开，格词法不得吞入换行。
pub(crate) struct TableInlineRow {
    pub(crate) ordinal: usize,
    pub(crate) start: usize,
    pub(crate) content_start: usize,
    pub(crate) content_end: usize,
    pub(crate) end: usize,
}

#[derive(Clone, Debug)]
/// 原 checkpoint 的一页共享 scope 与当前行标量；外部不能从裸 Range 构造真实行。
pub struct VerifiedTableInlineRow {
    checkpoint: TableCheckpoint,
    row: TableInlineRow,
}

impl TableCheckpoint {
    /// H 的边界来自原 Global 接受点，即使 body 已前移也不从 D 或容器缩进反推。
    pub fn header_inline_row(&self) -> VerifiedTableInlineRow {
        VerifiedTableInlineRow {
            checkpoint: self.clone(),
            row: self.header_inline,
        }
    }

    /// 仅导出这个 checkpoint 刚刚接受的一行；不会从调用者的 ordinal 或源范围补造历史行。
    pub fn last_inline_row(&self) -> Option<VerifiedTableInlineRow> {
        self.last_inline.map(|row| VerifiedTableInlineRow {
            checkpoint: self.clone(),
            row,
        })
    }
}

impl VerifiedTableInlineRow {
    pub fn row_ordinal(&self) -> usize {
        self.row.ordinal
    }

    pub fn source_range(&self) -> Range<usize> {
        self.row.start..self.row.end
    }

    /// math 是原同一行入口状态；扫描前列和原 pipe/escape 后，才捕获目标格自己的入口。
    pub fn cell_scanner(
        &self,
        column: usize,
        math: MathContext,
    ) -> Result<TableCellSemanticScanner, FrontierError> {
        if column >= self.checkpoint.scalars.columns {
            return Err(FrontierError::InvalidFrame);
        }
        Ok(TableCellSemanticScanner {
            row: self.clone(),
            column,
            scanner: TableCellScanner::new(self.row.content_start),
            next: self.row.content_start,
            math,
            escaped: false,
            cell_math: None,
            seed: None,
        })
    }
}

#[derive(Debug)]
/// 与原 Scanner 同一次逐 byte 调用共享 trim/pipe 结果；原调用方继续负责源 frame 与预算。
pub struct TableCellSemanticScanner {
    row: VerifiedTableInlineRow,
    column: usize,
    scanner: TableCellScanner,
    next: usize,
    math: MathContext,
    escaped: bool,
    cell_math: Option<MathContext>,
    seed: Option<VerifiedInlineSemanticSeed>,
}

impl TableCellSemanticScanner {
    pub fn cell_count(&self) -> usize {
        self.scanner.cell_count()
    }
    pub fn trimmed_range(&self) -> Range<usize> {
        self.scanner.trimmed_range()
    }
    pub fn trailing_pipe(&self) -> bool {
        self.scanner.trailing_pipe()
    }

    /// Core 已借原连续源片；本次先记真实格入口，再沿同一个原数学转移处理当前字节。
    pub fn push(&mut self, byte: u8) -> Option<TableCellBoundary> {
        let cells = self.scanner.cell_count();
        let boundary = self.scanner.push(byte);
        if self.scanner.cell_count() != cells && self.scanner.cell_count() == self.column + 1 {
            self.cell_math = Some(self.math);
        }
        if let Some(boundary) = &boundary {
            self.capture(boundary);
        }
        observe_math_byte(&mut self.math, &mut self.escaped, byte);
        self.next += 1;
        boundary
    }

    /// 必须到达原行内容末端才收尾；frame 末端不能签发 cell 或 EOF 权限。
    pub fn finish(&mut self) -> Option<TableCellBoundary> {
        assert_eq!(self.next, self.row.row.content_end);
        let boundary = self.scanner.finish();
        if let Some(boundary) = &boundary {
            self.capture(boundary);
        }
        boundary
    }

    /// 隐式缺格保持 None；显式零长格与非空格都必须由同一次原词法边界签发。
    pub fn take_semantic_seed(&mut self) -> Option<VerifiedInlineSemanticSeed> {
        self.seed.take()
    }

    /// 结构票只为这个实际列签发来源；Plain、Code、引用和 underline 仍须各自真正完成。
    fn capture(&mut self, boundary: &TableCellBoundary) {
        if self.scanner.cell_count() != self.column + 1 || self.seed.is_some() {
            return;
        }
        let checkpoint = &self.row.checkpoint;
        self.seed = Some(VerifiedInlineSemanticSeed {
            revision: checkpoint.scalars.source_revision,
            source_end: checkpoint.scalars.source_end,
            options: checkpoint.options,
            kind: InlineBlockKind::TableCell,
            physical_start: boundary.trimmed.start,
            opening: None,
            start: boundary.trimmed.start,
            end: boundary.trimmed.end,
            lexical_end: boundary.trimmed.end,
            last_content_end: boundary.trimmed.end,
            last_line_end: boundary.trimmed.end,
            spine: checkpoint.spine.clone(),
            cell: Some(crate::global_frontier::TableCellInlineContext {
                header: checkpoint.scalars.header_byte_start,
                row: self.row.row.ordinal,
                column: self.column,
                math: self.cell_math.expect("原 Scanner 已实际创建这个显式格"),
            }),
        });
    }
}
