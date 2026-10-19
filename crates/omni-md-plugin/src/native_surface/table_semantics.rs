//! 直接借用同一份已验证表格窗口；正文覆盖、单格显示和分隔行不能各自重建语义。
//!
//! 构造只验证 Core 的原源身份，不延长文档或 Native 对象生命周期。调用者仍须在原生
//! 调用前后守住原输入、选择、模式与几何票据，不能把这个借用当成新授权。

use std::ops::Range;

use omni_markdown_core::markdown_document_buffer::{
    MarkdownDocumentBuffer, MarkdownTableRequestError, VerifiedMarkdownTableWindow,
};
use omni_markdown_core::markdown_viewport_presentation::table_window::{
    MarkdownProjectedTableWindow, MarkdownTableWindowCell, MarkdownTableWindowCellSource,
    MarkdownTableWindowRow,
};
use omni_markdown_core::{
    MarkdownPresentationQuery, MarkdownPresentationRun, MarkdownPresentationStyles,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 正文行必须使用整表绝对 ordinal；离屏 H 只授予有限单格上下文读取。
pub(super) enum NativeMarkdownTableCellScope {
    PublishedRow { row_ordinal: usize },
    HeaderContext,
}

/// 只借用 Ready 的既有记录，不复制表格、源字符串、coverage 或样式向量。
pub(super) struct NativeMarkdownTableSemanticView<'window> {
    presentation: &'window MarkdownProjectedTableWindow,
    coverage: &'window [Range<usize>],
}

impl<'window> NativeMarkdownTableSemanticView<'window> {
    /// 两个 getter 都核同一源根；返回值只依赖 Ready，不把文档借用带进 Native。
    pub(super) fn new(
        window: &'window VerifiedMarkdownTableWindow,
        document: &MarkdownDocumentBuffer,
    ) -> Result<Self, MarkdownTableRequestError> {
        let presentation = window.presentation_for(document)?;
        let coverage = window.coverage_for(document)?;
        Ok(Self {
            presentation,
            coverage,
        })
    }

    /// 布局直接借用真实 rows/cells，不能再转换成 legacy 整表 prototype。
    pub(super) fn presentation(&self) -> &'window MarkdownProjectedTableWindow {
        self.presentation
    }

    /// 每段只表示实际发布的源；调用者不能取首尾包络来补出 H 到中段的省略行。
    pub(super) fn coverage(&self) -> &'window [Range<usize>] {
        self.coverage
    }

    /// 覆盖外的 None 表示无读取权限；覆盖内的无样式间隙仍返回精确范围。
    pub(super) fn query_published(&self, index: usize) -> Option<MarkdownPresentationQuery> {
        let coverage = containing_coverage(self.coverage, index)?;
        query_runs_in_range(&self.presentation.runs, coverage, index)
    }

    /// cell 身份来自同一 Ready；缺格与真实空格不会因显示都为空而失去类型区别。
    pub(super) fn cell(
        &self,
        scope: NativeMarkdownTableCellScope,
        column: usize,
    ) -> Option<NativeMarkdownTableSemanticCell<'window>> {
        if column >= self.presentation.column_count {
            // 陈旧列坐标不允许越过当前 header 已证明的列数。
            return None;
        }
        let (row, cell, runs) = match scope {
            NativeMarkdownTableCellScope::PublishedRow { row_ordinal } => {
                // row() 按绝对 ordinal 查找；本批 Vec 位置不能冒充整表身份。
                let row = self.presentation.row(row_ordinal)?;
                if !range_is_covered(self.coverage, &row.utf16_range) {
                    // 未进入真实公开 coverage 的记录不得被借作可绘制行。
                    return None;
                }
                let cell = self.presentation.cell(row_ordinal, column)?;
                (row, cell, self.presentation.runs.as_slice())
            }
            NativeMarkdownTableCellScope::HeaderContext => {
                if self.presentation.row(0).is_some_and(|row| row.is_header) {
                    // H 已发布时复用主 cell/run 及相同覆盖认证，不再准备一份 header。
                    return self.cell(
                        NativeMarkdownTableCellScope::PublishedRow { row_ordinal: 0 },
                        column,
                    );
                }
                // 离屏 H 的独立记录仅用于字体和列宽，不能加入正文 coverage 或隐藏 D。
                let context = &self.presentation.header_context;
                let cell = self.presentation.header_cells().get(column)?;
                (&context.row, cell, context.runs.as_slice())
            }
        };
        if cell.column_index != column || !cell_source_is_inside_row(&cell.source, &row.utf16_range)
        {
            // 即使外层坐标合法，也不能读取邻接 cell 或把行外插入锚点当成当前缺格。
            return None;
        }
        Some(NativeMarkdownTableSemanticCell { row, cell, runs })
    }

    /// 只有真实发布的 H 和完整 D 覆盖、且 D 每一段都隐藏，才允许一点行高和视觉 band。
    pub(super) fn delimiter_is_hidden(&self) -> bool {
        let Some(header) = self.presentation.row(0).filter(|row| row.is_header) else {
            // 离屏 header_context 不会因为带有 D 的结构坐标而取得隐藏正文权限。
            return false;
        };
        let delimiter = &self.presentation.delimiter_utf16_range;
        if header.utf16_range != self.presentation.header_utf16_range
            || !range_is_covered(self.coverage, &header.utf16_range)
            || delimiter.is_empty()
            || !range_is_covered(self.coverage, delimiter)
        {
            // 空、错误关联或仅部分覆盖的 delimiter 不能由两端样式推断整体隐藏。
            return false;
        }
        delimiter_runs_are_hidden(&self.presentation.runs, delimiter)
    }
}

/// 一个 cell 的借用权限；正文与离屏 H 使用各自的既有 runs，不产生派生表格模型。
pub(super) struct NativeMarkdownTableSemanticCell<'window> {
    row: &'window MarkdownTableWindowRow,
    cell: &'window MarkdownTableWindowCell,
    runs: &'window [MarkdownPresentationRun],
}

impl<'window> NativeMarkdownTableSemanticCell<'window> {
    /// 保留绝对 ordinal 和物理行元数据，布局不需反查全表。
    pub(super) fn row(&self) -> &'window MarkdownTableWindowRow {
        self.row
    }

    /// 对齐和列身份直接来自原 cell，不能从显示文字或 Vec 位置重新猜测。
    pub(super) fn metadata(&self) -> &'window MarkdownTableWindowCell {
        self.cell
    }

    /// Present(empty) 仍是原显式空格；ImplicitEmpty 仍只有插入锚点。
    pub(super) fn source(&self) -> &'window MarkdownTableWindowCellSource {
        &self.cell.source
    }

    /// 单格的 scalar 样式查询可供现有显示转换闭包使用，范围不跨出该格真实源。
    pub(super) fn query(&self, index: usize) -> Option<MarkdownPresentationQuery> {
        match &self.cell.source {
            MarkdownTableWindowCellSource::Present(range) => {
                // 显式空格没有可读字符，自然返回 None，而不捏造相邻字符的样式。
                query_runs_in_range(self.runs, range, index)
            }
            MarkdownTableWindowCellSource::ImplicitEmpty { .. } => {
                // 缺格只有结构锚点；它没有可用于读取正文或隐藏字符的 interval。
                None
            }
        }
    }
}

/// coverage 已由 producer 排序；只返回包含 index 的一个真实段，不合并相邻或省略段。
fn containing_coverage(coverage: &[Range<usize>], index: usize) -> Option<&Range<usize>> {
    let next = coverage.partition_point(|range| range.end <= index);
    coverage.get(next).filter(|range| range.contains(&index))
}

/// 源行和 D 的完整内容必须由同一个公开段覆盖，空范围不增加读字符权限。
fn range_is_covered(coverage: &[Range<usize>], range: &Range<usize>) -> bool {
    if range.start > range.end {
        // 反向坐标不是合法空范围，也不能通过边界相等绕过覆盖检查。
        return false;
    }
    containing_coverage(coverage, range.start).is_some_and(|coverage| range.end <= coverage.end)
}

/// 所有返回值都受给定源 interval 截断；未命中 run 时仍保留前后 run 的精确 gap 边界。
fn query_runs_in_range(
    runs: &[MarkdownPresentationRun],
    permitted: &Range<usize>,
    index: usize,
) -> Option<MarkdownPresentationQuery> {
    if !permitted.contains(&index) {
        // coverage 外、cell 外与空范围均没有字符样式权限。
        return None;
    }
    let candidate = runs.partition_point(|run| run.utf16_range.end <= index);
    if let Some(run) = runs.get(candidate)
        && run.utf16_range.start <= index
    {
        // 一个 run 即使跨相邻 coverage，也只能返回当前获准源段内的交集。
        return Some(MarkdownPresentationQuery {
            utf16_range: run.utf16_range.start.max(permitted.start)
                ..run.utf16_range.end.min(permitted.end),
            styles: run.styles,
        });
    }
    // 默认样式也是有限语义结果；不能向前后延伸覆盖未准备的邻格或省略行。
    let start = candidate
        .checked_sub(1)
        .and_then(|previous| runs.get(previous))
        .map_or(permitted.start, |run| {
            run.utf16_range.end.max(permitted.start)
        });
    let end = runs.get(candidate).map_or(permitted.end, |run| {
        run.utf16_range.start.min(permitted.end)
    });
    Some(MarkdownPresentationQuery {
        utf16_range: start..end,
        styles: MarkdownPresentationStyles::default(),
    })
}

/// 只读取与目标有限 interval 相交的 runs；两端隐藏不能替代中间每个字符的证明。
fn delimiter_runs_are_hidden(runs: &[MarkdownPresentationRun], range: &Range<usize>) -> bool {
    if range.is_empty() {
        // 没有真实字符就没有可隐藏的 delimiter，反向范围同样不能获得权限。
        return false;
    }
    let mut index = runs.partition_point(|run| run.utf16_range.end <= range.start);
    let mut cursor = range.start;
    while cursor < range.end {
        // 一次二分后只沿当前有限范围单调推进；省略段、默认 gap 与显露段都立即失败。
        let Some(run) = runs.get(index) else {
            return false;
        };
        if run.utf16_range.start > cursor
            || run.utf16_range.end <= cursor
            || !run
                .styles
                .contains(MarkdownPresentationStyles::TABLE_DELIMITER)
            || !run
                .styles
                .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
        {
            // 不跨越缺失区间，不把无进度或只带部分角色的 run 当作整段隐藏。
            return false;
        }
        cursor = run.utf16_range.end.min(range.end);
        index += 1;
    }
    true
}

/// 这里只验证原 cell 的有限源归属，不把缺格锚点转成普通源替换范围。
fn cell_source_is_inside_row(source: &MarkdownTableWindowCellSource, row: &Range<usize>) -> bool {
    match source {
        MarkdownTableWindowCellSource::Present(range) => {
            // 两端相等是合法显式空格；非空与空格都必须留在原真实行内容里。
            row.start <= range.start && range.start <= range.end && range.end <= row.end
        }
        MarkdownTableWindowCellSource::ImplicitEmpty {
            insertion_anchor_utf16,
        } => {
            // 行末锚点合法，下一行或前一行的锚点没有当前 cell 权限。
            row.start <= *insertion_anchor_utf16 && *insertion_anchor_utf16 <= row.end
        }
    }
}

#[cfg(test)]
mod tests;
